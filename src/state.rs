use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use libsql::Builder;
use reqwest::Client;
use tokio::sync::RwLock;
use tracing::{debug, error, info};

use crate::alma_ws::{AlmaEvent, AlmaWsClient};
use crate::config::Config;

/// Identifies a QQ chat target for bidirectional forwarding.
#[derive(Clone, Debug)]
pub struct QqTarget {
    pub target_type: String, // "private" or "group"
    pub target_id: i64,      // user_id or group_id
}

/// A single group chat message stored in the in-memory history buffer.
#[derive(Clone, Debug)]
pub struct GroupMessage {
    pub display_name: String,
    pub text: String,
    pub timestamp: u64,
}

#[derive(Clone, Debug)]
struct SentReply {
    text: String,
    sent_at: Instant,
}

/// Application-wide shared state.
pub struct AppState {
    pub http_client: Client,
    pub config: Config,
    pub db: libsql::Database,
    pub default_model: RwLock<Option<String>>,
    pub alma_ws: RwLock<Option<AlmaWsClient>>,
    /// Reverse lookup: Alma thread_id → QQ session key (for bidirectional forwarding)
    pub session_reverse: RwLock<HashMap<String, String>>,
    /// Recent outgoing reply texts per thread (for dedup in bidirectional mode)
    sent_replies: RwLock<HashMap<String, VecDeque<SentReply>>>,
    /// In-memory group chat history per session key (for ephemeral context injection)
    pub group_history: RwLock<HashMap<String, VecDeque<GroupMessage>>>,
    /// Cached QQ group titles keyed by numeric group_id.
    pub group_titles: RwLock<HashMap<i64, String>>,
    /// Broadcast channel: Alma GUI events → OneBot handler (for bidirectional forwarding)
    pub alma_event_tx: tokio::sync::broadcast::Sender<AlmaEvent>,
}

/// Cheap-to-clone wrapper around `Arc<AppState>`.
#[derive(Clone)]
pub struct SharedState(Arc<AppState>);

impl std::ops::Deref for SharedState {
    type Target = AppState;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl SharedState {
    pub async fn new(config: Config) -> Result<Self, String> {
        let db_path = config.db_path.to_string_lossy().to_string();
        info!("Opening Turso DB: {}", db_path);

        let db = Builder::new_local(&db_path)
            .build()
            .await
            .map_err(|e| format!("Failed to open database: {}", e))?;

        // Create tables if they don't exist
        let conn = db
            .connect()
            .map_err(|e| format!("DB connect failed: {}", e))?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS threads (
                session_key TEXT PRIMARY KEY,
                thread_id TEXT NOT NULL
            )",
            (),
        )
        .await
        .map_err(|e| format!("Failed to create threads table: {}", e))?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS profiles (
                user_id TEXT PRIMARY KEY,
                profile_name TEXT NOT NULL
            )",
            (),
        )
        .await
        .map_err(|e| format!("Failed to create profiles table: {}", e))?;

        let (alma_event_tx, _) = tokio::sync::broadcast::channel(64);

        Ok(SharedState(Arc::new(AppState {
            http_client: Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("Failed to build HTTP client"),
            config,
            db,
            default_model: RwLock::new(None),
            alma_ws: RwLock::new(None),
            session_reverse: RwLock::new(HashMap::new()),
            sent_replies: RwLock::new(HashMap::new()),
            group_history: RwLock::new(HashMap::new()),
            group_titles: RwLock::new(HashMap::new()),
            alma_event_tx,
        })))
    }

    // ── Thread map ───────────────────────────────────────────────────────

    pub async fn get_thread_id(&self, session_key: &str) -> Option<String> {
        let conn = match self.db.connect() {
            Ok(c) => c,
            Err(e) => {
                error!("DB connect error: {}", e);
                return None;
            }
        };

        let stmt = match conn
            .prepare("SELECT thread_id FROM threads WHERE session_key = ?1")
            .await
        {
            Ok(s) => s,
            Err(e) => {
                error!("DB prepare error: {}", e);
                return None;
            }
        };

        let mut rows = match stmt.query([session_key]).await {
            Ok(r) => r,
            Err(e) => {
                error!("DB query error: {}", e);
                return None;
            }
        };

        match rows.next().await {
            Ok(Some(row)) => {
                let tid = row.get::<String>(0).ok()?;
                // Update reverse map inline (fast RwLock write, no spawn needed)
                self.session_reverse
                    .write()
                    .await
                    .insert(tid.clone(), session_key.to_string());
                Some(tid)
            }
            _ => None,
        }
    }

    pub async fn set_thread_id(&self, session_key: String, thread_id: String) {
        // Populate reverse map
        self.session_reverse
            .write()
            .await
            .insert(thread_id.clone(), session_key.clone());

        let conn = match self.db.connect() {
            Ok(c) => c,
            Err(e) => {
                error!("DB connect error: {}", e);
                return;
            }
        };

        match conn
            .prepare("INSERT OR REPLACE INTO threads (session_key, thread_id) VALUES (?1, ?2)")
            .await
        {
            Ok(stmt) => {
                if let Err(e) = stmt
                    .execute([session_key.as_str(), thread_id.as_str()])
                    .await
                {
                    error!("DB insert error: {}", e);
                } else {
                    debug!("Thread saved: {} → {}", session_key, thread_id);
                }
            }
            Err(e) => error!("DB prepare error: {}", e),
        }
    }

    /// Look up the QQ target for a given Alma thread ID (for bidirectional forwarding).
    pub async fn get_qq_target(&self, thread_id: &str) -> Option<QqTarget> {
        let session_key =
            if let Some(session_key) = self.session_reverse.read().await.get(thread_id).cloned() {
                session_key
            } else {
                let conn = match self.db.connect() {
                    Ok(c) => c,
                    Err(e) => {
                        error!("DB connect error: {}", e);
                        return None;
                    }
                };

                let stmt = match conn
                    .prepare("SELECT session_key FROM threads WHERE thread_id = ?1")
                    .await
                {
                    Ok(s) => s,
                    Err(e) => {
                        error!("DB prepare error: {}", e);
                        return None;
                    }
                };

                let mut rows = match stmt.query([thread_id]).await {
                    Ok(r) => r,
                    Err(e) => {
                        error!("DB query error: {}", e);
                        return None;
                    }
                };

                let session_key = match rows.next().await {
                    Ok(Some(row)) => row.get::<String>(0).ok()?,
                    _ => return None,
                };

                self.session_reverse
                    .write()
                    .await
                    .insert(thread_id.to_string(), session_key.clone());

                session_key
            };

        let parts: Vec<&str> = session_key.splitn(2, ':').collect();
        if parts.len() != 2 {
            return None;
        }

        let target_type = parts[0].to_string();
        let target_id: i64 = parts[1].parse().ok()?;

        Some(QqTarget {
            target_type,
            target_id,
        })
    }

    // ── Profile map ──────────────────────────────────────────────────────

    pub async fn has_profile(&self, user_id: &str) -> bool {
        let conn = match self.db.connect() {
            Ok(c) => c,
            Err(_) => return false,
        };

        let stmt = match conn
            .prepare("SELECT 1 FROM profiles WHERE user_id = ?1")
            .await
        {
            Ok(s) => s,
            Err(_) => return false,
        };

        match stmt.query([user_id]).await {
            Ok(mut rows) => rows.next().await.ok().flatten().is_some(),
            Err(_) => false,
        }
    }

    pub async fn set_profile(&self, user_id: String, profile_name: String) {
        let conn = match self.db.connect() {
            Ok(c) => c,
            Err(e) => {
                error!("DB connect error: {}", e);
                return;
            }
        };

        match conn
            .prepare("INSERT OR REPLACE INTO profiles (user_id, profile_name) VALUES (?1, ?2)")
            .await
        {
            Ok(stmt) => {
                if let Err(e) = stmt
                    .execute([user_id.as_str(), profile_name.as_str()])
                    .await
                {
                    error!("DB insert error: {}", e);
                } else {
                    debug!("Profile saved: {} → {}", user_id, profile_name);
                }
            }
            Err(e) => error!("DB prepare error: {}", e),
        }
    }

    // ── Alma WS client ───────────────────────────────────────────────────

    pub async fn set_alma_ws(&self, client: AlmaWsClient) {
        *self.alma_ws.write().await = Some(client);
    }

    pub async fn get_alma_ws(&self) -> Option<AlmaWsClient> {
        self.alma_ws.read().await.clone()
    }

    // ── Default model ────────────────────────────────────────────────────

    pub async fn set_default_model(&self, model: String) {
        *self.default_model.write().await = Some(model);
    }

    pub async fn get_default_model(&self) -> Option<String> {
        // Bootstrap/fallback model priority for new threads or API fallback paths.
        if let Some(ref m) = self.config.alma_model {
            return Some(m.clone());
        }
        self.default_model.read().await.clone()
    }

    // ── Reply dedup (for bidirectional forwarding) ───────────────────────

    /// Register a reply we sent to QQ (so we don't re-forward it from Alma).
    pub async fn register_sent_reply(&self, thread_id: &str, text: &str) {
        let mut map = self.sent_replies.write().await;
        let deque = map
            .entry(thread_id.to_string())
            .or_insert_with(VecDeque::new);
        deque.push_back(SentReply {
            text: text.to_string(),
            sent_at: Instant::now(),
        });
        while deque.len() > 20 {
            deque.pop_front();
        }
    }

    /// Check if a text was recently sent as a reply (for dedup).
    /// Short identical replies are deduped within a narrow time window to avoid
    /// double-sending the same Alma reply via both the direct send path and the
    /// later `message_updated` event. Longer replies also allow prefix matching
    /// to cover chunking differences.
    pub async fn was_sent_recently(&self, thread_id: &str, text: &str) -> bool {
        let mut map = self.sent_replies.write().await;
        if let Some(deque) = map.get_mut(thread_id) {
            while let Some(front) = deque.front() {
                if front.sent_at.elapsed() > Duration::from_secs(15) {
                    deque.pop_front();
                } else {
                    break;
                }
            }

            return deque
                .iter()
                .any(|sent| sent_reply_matches(&sent.text, text));
        }
        false
    }

    // ── Group chat history (in-memory ring buffer) ──────────────────────

    /// Record a group message in the in-memory history buffer.
    /// Respects `config.group_history_size` — if 0, does nothing.
    pub async fn record_group_message(&self, session_key: &str, msg: GroupMessage) {
        let max_size = self.config.group_history_size;
        if max_size == 0 {
            return;
        }

        let mut map = self.group_history.write().await;
        let deque = map
            .entry(session_key.to_string())
            .or_insert_with(VecDeque::new);
        deque.push_back(msg);
        while deque.len() > max_size {
            deque.pop_front();
        }
    }

    /// Get recent group chat history for ephemeral context injection.
    /// Returns an empty Vec if history is disabled or no messages recorded.
    pub async fn get_group_history(&self, session_key: &str) -> Vec<GroupMessage> {
        let map = self.group_history.read().await;
        map.get(session_key)
            .map(|deque| deque.iter().cloned().collect())
            .unwrap_or_default()
    }

    pub async fn set_group_title(&self, group_id: i64, title: String) {
        self.group_titles.write().await.insert(group_id, title);
    }

    pub async fn get_group_title(&self, group_id: i64) -> Option<String> {
        self.group_titles.read().await.get(&group_id).cloned()
    }
}

/// Safely truncate a string prefix by bytes without panicking on UTF-8 boundaries.
/// Walks backwards from `max_bytes` to the nearest char boundary.
fn safe_prefix(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn sent_reply_matches(sent: &str, candidate: &str) -> bool {
    if sent == candidate {
        return true;
    }

    let sent_prefix = safe_prefix(sent, 100);
    let candidate_prefix = safe_prefix(candidate, 100);
    let min_match_len: usize = 30;

    sent_prefix == candidate_prefix
        && sent_prefix.len() >= min_match_len
        && candidate_prefix.len() >= min_match_len
}

#[cfg(test)]
mod tests {
    use super::{safe_prefix, sent_reply_matches};

    #[test]
    fn safe_prefix_keeps_utf8_boundaries() {
        let prefix = safe_prefix("你好世界你好世界", 7);
        assert!(!prefix.contains('\u{fffd}'));
        assert!(prefix.len() <= 7);
    }

    #[test]
    fn sent_reply_matches_short_exact_cjk_reply() {
        assert!(sent_reply_matches("萌依收到电报", "萌依收到电报"));
    }

    #[test]
    fn sent_reply_matches_rejects_short_partial_match() {
        assert!(!sent_reply_matches("萌依收到电报", "萌依收到"));
    }

    #[test]
    fn sent_reply_matches_long_prefix_equivalent_chunks() {
        let text = "这是一个足够长的回复，用来验证前缀匹配在长文本场景下仍然有效，而且不会被 UTF-8 截断搞坏。";
        let prefix_equivalent = "这是一个足够长的回复，用来验证前缀匹配在长文本场景下仍然有效，而且不会被 UTF-8 截断搞坏。后续补充";
        assert!(sent_reply_matches(text, prefix_equivalent));
    }
}
