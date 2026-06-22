use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use reqwest::Client;
use serde::Serialize;
use tokio::sync::RwLock;
use tracing::{debug, error, info};
use turso::Builder;

use crate::alma_ws::{AlmaEvent, AlmaWsClient};
use crate::config::Config;
use crate::onebot::OneBotApiHandle;

const SESSION_REVERSE_LIMIT: usize = 4096;

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
    pub message_id: Option<i64>,
    pub is_bot: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct GroupMember {
    pub group_id: i64,
    pub user_id: i64,
    pub display_name: String,
    pub last_seen: u64,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct GroupDirectoryEntry {
    pub group_id: i64,
    pub title: Option<String>,
    pub last_active: u64,
    pub members: Vec<GroupMember>,
}

impl GroupDirectoryEntry {
    pub fn unknown(group_id: i64) -> Self {
        Self {
            group_id,
            title: None,
            last_active: 0,
            members: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
struct SentReply {
    text: String,
    sent_at: Instant,
}

/// Application-wide shared state.
pub struct AppState {
    pub http_client: Client,
    pub config: RwLock<Config>,
    pub db: turso::Database,
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
    /// Cached People Profile paths keyed by QQ user_id string.
    pub people_profile_paths: RwLock<HashMap<String, PathBuf>>,
    /// Active OneBot reverse-WS API path for HTTP command endpoints.
    onebot_api: RwLock<Option<OneBotApiHandle>>,
    /// Broadcast channel: Alma GUI events → OneBot handler (for bidirectional forwarding)
    pub alma_event_tx: tokio::sync::broadcast::Sender<AlmaEvent>,
    /// Monotonic generation for active OneBot reverse WS connections.
    /// Only the newest connection may forward Alma GUI events to QQ.
    onebot_connection_epoch: AtomicU64,
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

        conn.execute(
            "CREATE TABLE IF NOT EXISTS groups (
                group_id TEXT PRIMARY KEY,
                title TEXT NOT NULL DEFAULT '',
                last_active TEXT NOT NULL DEFAULT '0'
            )",
            (),
        )
        .await
        .map_err(|e| format!("Failed to create groups table: {}", e))?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS group_members (
                group_id TEXT NOT NULL,
                user_id TEXT NOT NULL,
                display_name TEXT NOT NULL,
                last_seen TEXT NOT NULL DEFAULT '0',
                PRIMARY KEY (group_id, user_id)
            )",
            (),
        )
        .await
        .map_err(|e| format!("Failed to create group_members table: {}", e))?;

        let (alma_event_tx, _) = tokio::sync::broadcast::channel(64);

        Ok(SharedState(Arc::new(AppState {
            http_client: Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("Failed to build HTTP client"),
            config: RwLock::new(config),
            db,
            default_model: RwLock::new(None),
            alma_ws: RwLock::new(None),
            session_reverse: RwLock::new(HashMap::new()),
            sent_replies: RwLock::new(HashMap::new()),
            group_history: RwLock::new(HashMap::new()),
            group_titles: RwLock::new(HashMap::new()),
            people_profile_paths: RwLock::new(HashMap::new()),
            onebot_api: RwLock::new(None),
            alma_event_tx,
            onebot_connection_epoch: AtomicU64::new(0),
        })))
    }

    // ── Thread map ───────────────────────────────────────────────────────

    pub async fn get_thread_id(&self, session_key: &str) -> Result<Option<String>, String> {
        let conn = match self.db.connect() {
            Ok(c) => c,
            Err(e) => {
                error!("DB connect error: {}", e);
                return Err(format!("DB connect error: {}", e));
            }
        };

        let mut stmt = match conn
            .prepare("SELECT thread_id FROM threads WHERE session_key = ?1")
            .await
        {
            Ok(s) => s,
            Err(e) => {
                error!("DB prepare error: {}", e);
                return Err(format!("DB prepare error: {}", e));
            }
        };

        let mut rows = match stmt.query([session_key]).await {
            Ok(r) => r,
            Err(e) => {
                error!("DB query error: {}", e);
                return Err(format!("DB query error: {}", e));
            }
        };

        match rows.next().await {
            Ok(Some(row)) => {
                let tid = row
                    .get::<String>(0)
                    .map_err(|e| format!("DB row decode error: {}", e))?;
                self.cache_thread_mapping(tid.clone(), session_key.to_string())
                    .await;
                Ok(Some(tid))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(format!("DB row error: {}", e)),
        }
    }

    pub async fn set_thread_id(
        &self,
        session_key: String,
        thread_id: String,
    ) -> Result<(), String> {
        self.cache_thread_mapping(thread_id.clone(), session_key.clone())
            .await;

        let conn = match self.db.connect() {
            Ok(c) => c,
            Err(e) => {
                error!("DB connect error: {}", e);
                return Err(format!("DB connect error: {}", e));
            }
        };

        match conn
            .prepare("INSERT OR REPLACE INTO threads (session_key, thread_id) VALUES (?1, ?2)")
            .await
        {
            Ok(mut stmt) => {
                if let Err(e) = stmt
                    .execute([session_key.as_str(), thread_id.as_str()])
                    .await
                {
                    error!("DB insert error: {}", e);
                    return Err(format!("DB insert error: {}", e));
                } else {
                    debug!("Thread saved: {} → {}", session_key, thread_id);
                }
            }
            Err(e) => {
                error!("DB prepare error: {}", e);
                return Err(format!("DB prepare error: {}", e));
            }
        }
        Ok(())
    }

    /// Look up the QQ target for a given Alma thread ID (for bidirectional forwarding).
    pub async fn get_qq_target(&self, thread_id: &str) -> Result<Option<QqTarget>, String> {
        let session_key =
            if let Some(session_key) = self.session_reverse.read().await.get(thread_id).cloned() {
                session_key
            } else {
                let conn = match self.db.connect() {
                    Ok(c) => c,
                    Err(e) => {
                        error!("DB connect error: {}", e);
                        return Err(format!("DB connect error: {}", e));
                    }
                };

                let mut stmt = match conn
                    .prepare("SELECT session_key FROM threads WHERE thread_id = ?1")
                    .await
                {
                    Ok(s) => s,
                    Err(e) => {
                        error!("DB prepare error: {}", e);
                        return Err(format!("DB prepare error: {}", e));
                    }
                };

                let mut rows = match stmt.query([thread_id]).await {
                    Ok(r) => r,
                    Err(e) => {
                        error!("DB query error: {}", e);
                        return Err(format!("DB query error: {}", e));
                    }
                };

                let session_key = match rows.next().await {
                    Ok(Some(row)) => row
                        .get::<String>(0)
                        .map_err(|e| format!("DB row decode error: {}", e))?,
                    Ok(None) => return Ok(None),
                    Err(e) => return Err(format!("DB row error: {}", e)),
                };

                self.cache_thread_mapping(thread_id.to_string(), session_key.clone())
                    .await;

                session_key
            };

        let parts: Vec<&str> = session_key.splitn(2, ':').collect();
        if parts.len() != 2 {
            return Ok(None);
        }

        let target_type = parts[0].to_string();
        let target_id: i64 = match parts[1].parse() {
            Ok(id) => id,
            Err(e) => return Err(format!("Invalid session target id '{}': {}", parts[1], e)),
        };

        Ok(Some(QqTarget {
            target_type,
            target_id,
        }))
    }

    async fn cache_thread_mapping(&self, thread_id: String, session_key: String) {
        let mut reverse = self.session_reverse.write().await;
        reverse.insert(thread_id, session_key);
        if reverse.len() <= SESSION_REVERSE_LIMIT {
            return;
        }

        let excess = reverse.len().saturating_sub(SESSION_REVERSE_LIMIT);
        let keys: Vec<String> = reverse.keys().take(excess).cloned().collect();
        for key in keys {
            reverse.remove(&key);
        }
    }

    // ── Profile map ──────────────────────────────────────────────────────

    pub async fn has_profile(&self, user_id: &str) -> Result<bool, String> {
        let conn = match self.db.connect() {
            Ok(c) => c,
            Err(e) => return Err(format!("DB connect error: {}", e)),
        };

        let mut stmt = match conn
            .prepare("SELECT 1 FROM profiles WHERE user_id = ?1")
            .await
        {
            Ok(s) => s,
            Err(e) => return Err(format!("DB prepare error: {}", e)),
        };

        match stmt.query([user_id]).await {
            Ok(mut rows) => rows
                .next()
                .await
                .map(|row| row.is_some())
                .map_err(|e| format!("DB row error: {}", e)),
            Err(e) => Err(format!("DB query error: {}", e)),
        }
    }

    pub async fn set_profile(&self, user_id: String, profile_name: String) -> Result<(), String> {
        let conn = match self.db.connect() {
            Ok(c) => c,
            Err(e) => {
                error!("DB connect error: {}", e);
                return Err(format!("DB connect error: {}", e));
            }
        };

        match conn
            .prepare("INSERT OR REPLACE INTO profiles (user_id, profile_name) VALUES (?1, ?2)")
            .await
        {
            Ok(mut stmt) => {
                if let Err(e) = stmt
                    .execute([user_id.as_str(), profile_name.as_str()])
                    .await
                {
                    error!("DB insert error: {}", e);
                    return Err(format!("DB insert error: {}", e));
                } else {
                    debug!("Profile saved: {} → {}", user_id, profile_name);
                }
            }
            Err(e) => {
                error!("DB prepare error: {}", e);
                return Err(format!("DB prepare error: {}", e));
            }
        }
        Ok(())
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
        let cfg = self.config.read().await;
        if let Some(ref m) = cfg.alma_model {
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
        let max_size = self.config.read().await.group_history_size;
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

    pub async fn touch_group(
        &self,
        group_id: i64,
        title: Option<&str>,
        timestamp_secs: u64,
    ) -> Result<(), String> {
        let timestamp = normalize_timestamp(timestamp_secs).to_string();
        let title = title.map(str::trim).filter(|t| !t.is_empty()).unwrap_or("");
        if !title.is_empty() {
            self.group_titles
                .write()
                .await
                .insert(group_id, title.to_string());
        }

        let conn = self
            .db
            .connect()
            .map_err(|e| format!("DB connect error: {}", e))?;
        let group_id = group_id.to_string();

        let mut insert = conn
            .prepare(
                "INSERT OR IGNORE INTO groups (group_id, title, last_active)
                 VALUES (?1, ?2, ?3)",
            )
            .await
            .map_err(|e| format!("DB prepare error: {}", e))?;
        insert
            .execute([group_id.as_str(), title, timestamp.as_str()])
            .await
            .map_err(|e| format!("DB insert error: {}", e))?;

        let mut update = conn
            .prepare(
                "UPDATE groups
                 SET title = CASE WHEN ?2 <> '' THEN ?2 ELSE title END,
                     last_active = ?3
                 WHERE group_id = ?1",
            )
            .await
            .map_err(|e| format!("DB prepare error: {}", e))?;
        update
            .execute([group_id.as_str(), title, timestamp.as_str()])
            .await
            .map_err(|e| format!("DB update error: {}", e))?;

        Ok(())
    }

    pub async fn record_group_member(
        &self,
        group_id: i64,
        user_id: i64,
        display_name: &str,
        timestamp_secs: u64,
    ) -> Result<(), String> {
        let timestamp = normalize_timestamp(timestamp_secs).to_string();
        let display_name = display_name.trim();
        let display_name = if display_name.is_empty() {
            format!("QQ user {}", user_id)
        } else {
            display_name.to_string()
        };

        let conn = self
            .db
            .connect()
            .map_err(|e| format!("DB connect error: {}", e))?;
        let group_id = group_id.to_string();
        let user_id = user_id.to_string();

        let mut insert = conn
            .prepare(
                "INSERT OR REPLACE INTO group_members
                 (group_id, user_id, display_name, last_seen)
                 VALUES (?1, ?2, ?3, ?4)",
            )
            .await
            .map_err(|e| format!("DB prepare error: {}", e))?;
        insert
            .execute([
                group_id.as_str(),
                user_id.as_str(),
                display_name.as_str(),
                timestamp.as_str(),
            ])
            .await
            .map_err(|e| format!("DB insert error: {}", e))?;

        Ok(())
    }

    pub async fn group_directory_snapshot(&self) -> Result<Vec<GroupDirectoryEntry>, String> {
        let conn = self
            .db
            .connect()
            .map_err(|e| format!("DB connect error: {}", e))?;
        let mut groups = HashMap::<i64, GroupDirectoryEntry>::new();

        let mut stmt = conn
            .prepare("SELECT group_id, title, last_active FROM groups")
            .await
            .map_err(|e| format!("DB prepare error: {}", e))?;
        let mut rows = stmt
            .query(())
            .await
            .map_err(|e| format!("DB query error: {}", e))?;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| format!("DB row error: {}", e))?
        {
            let group_id = row
                .get::<String>(0)
                .map_err(|e| format!("DB get group_id error: {}", e))?
                .parse::<i64>()
                .map_err(|e| format!("Invalid group_id in DB: {}", e))?;
            let title = row
                .get::<String>(1)
                .map_err(|e| format!("DB get title error: {}", e))?;
            let last_active = row
                .get::<String>(2)
                .map_err(|e| format!("DB get last_active error: {}", e))?
                .parse::<u64>()
                .map_err(|e| format!("Invalid last_active in DB: {}", e))?;
            groups.insert(
                group_id,
                GroupDirectoryEntry {
                    group_id,
                    title: if title.trim().is_empty() {
                        None
                    } else {
                        Some(title)
                    },
                    last_active,
                    members: Vec::new(),
                },
            );
        }

        let mut member_stmt = conn
            .prepare(
                "SELECT group_id, user_id, display_name, last_seen
                 FROM group_members
                 ORDER BY group_id ASC, last_seen DESC",
            )
            .await
            .map_err(|e| format!("DB prepare error: {}", e))?;
        let mut member_rows = member_stmt
            .query(())
            .await
            .map_err(|e| format!("DB query error: {}", e))?;
        while let Some(row) = member_rows
            .next()
            .await
            .map_err(|e| format!("DB row error: {}", e))?
        {
            let group_id = row
                .get::<String>(0)
                .map_err(|e| format!("DB get member group_id error: {}", e))?
                .parse::<i64>()
                .map_err(|e| format!("Invalid member group_id in DB: {}", e))?;
            let user_id = row
                .get::<String>(1)
                .map_err(|e| format!("DB get member user_id error: {}", e))?
                .parse::<i64>()
                .map_err(|e| format!("Invalid member user_id in DB: {}", e))?;
            let display_name = row
                .get::<String>(2)
                .map_err(|e| format!("DB get display_name error: {}", e))?;
            let last_seen = row
                .get::<String>(3)
                .map_err(|e| format!("DB get last_seen error: {}", e))?
                .parse::<u64>()
                .map_err(|e| format!("Invalid last_seen in DB: {}", e))?;

            groups
                .entry(group_id)
                .or_insert_with(|| GroupDirectoryEntry::unknown(group_id))
                .members
                .push(GroupMember {
                    group_id,
                    user_id,
                    display_name,
                    last_seen,
                });
        }

        for (group_id, title) in self.group_titles.read().await.iter() {
            groups
                .entry(*group_id)
                .or_insert_with(|| GroupDirectoryEntry::unknown(*group_id))
                .title = Some(title.clone());
        }

        let mut entries: Vec<_> = groups.into_values().collect();
        entries.sort_by_key(|entry| entry.group_id);
        Ok(entries)
    }

    // ── OneBot connection ownership ─────────────────────────────────────

    pub fn register_onebot_connection(&self) -> u64 {
        self.onebot_connection_epoch.fetch_add(1, Ordering::AcqRel) + 1
    }

    pub fn is_current_onebot_connection(&self, connection_id: u64) -> bool {
        self.onebot_connection_epoch.load(Ordering::Acquire) == connection_id
    }

    pub async fn set_onebot_api_handle(&self, handle: OneBotApiHandle) {
        *self.onebot_api.write().await = Some(handle);
    }

    pub async fn get_onebot_api_handle(&self) -> Option<OneBotApiHandle> {
        let handle = self.onebot_api.read().await.clone()?;
        if self.is_current_onebot_connection(handle.connection_id) {
            Some(handle)
        } else {
            None
        }
    }

    pub async fn clear_onebot_api_handle(&self, connection_id: u64) {
        let mut handle = self.onebot_api.write().await;
        if handle
            .as_ref()
            .map(|current| current.connection_id == connection_id)
            .unwrap_or(false)
        {
            *handle = None;
        }
    }

    pub async fn has_onebot_api_handle(&self) -> bool {
        self.get_onebot_api_handle().await.is_some()
    }
}

fn normalize_timestamp(timestamp_secs: u64) -> u64 {
    if timestamp_secs != 0 {
        return timestamp_secs;
    }
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
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
    use super::{SharedState, safe_prefix, sent_reply_matches};
    use crate::config::Config;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_db_path(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("alma-onebot-bridge-{name}-{nonce}.db"))
    }

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

    #[tokio::test]
    async fn onebot_connection_epoch_keeps_only_latest_current() {
        let db_path = temp_db_path("connection-epoch");
        let mut config = Config::load();
        config.db_path = db_path.clone();
        let state = SharedState::new(config).await.unwrap();

        let first = state.register_onebot_connection();
        let second = state.register_onebot_connection();

        assert!(!state.is_current_onebot_connection(first));
        assert!(state.is_current_onebot_connection(second));

        let _ = std::fs::remove_file(db_path);
    }
}
