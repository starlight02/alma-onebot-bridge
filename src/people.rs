use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tracing::{debug, info, warn};

use crate::onebot::event::Sender;
use crate::onebot::{PendingCalls, call_api};
use crate::state::SharedState;
use tokio::sync::mpsc;
use warp::ws::Message;

#[derive(Clone, Debug, PartialEq, Eq)]
struct GroupCardInfo {
    card: String,
}

/// Ensure a People Profile exists for the given QQ user.
/// If the profile doesn't exist, fetch user info from OneBot and create it.
/// If it already exists and we have group context, update the group card (群名片).
///
/// `group_id` — Some(group_id) when the message came from a group chat.
pub async fn ensure_people_profile(
    user_id: i64,
    sender: Option<&Sender>,
    group_id: Option<i64>,
    state: &SharedState,
    ws_tx: &mpsc::UnboundedSender<Message>,
    pending: &PendingCalls,
) {
    let user_id_str = user_id.to_string();

    // Extract group card (群名片) — only present in group chat context
    let card = sender
        .and_then(|s| s.card.as_deref())
        .filter(|c| !c.is_empty());
    let group_id_str = group_id.map(|g| g.to_string());

    let profile_path = resolve_profile_path_for_user(state, user_id).await;
    let preferred_name = sender
        .and_then(|s| s.nickname.as_deref())
        .filter(|n| !n.is_empty())
        .or(card);

    // ── Existing profile on disk: update group card if applicable ──────
    if profile_path.exists() {
        match sync_profile_file_async(
            profile_path.clone(),
            user_id,
            preferred_name.map(str::to_string),
            group_id_str.clone(),
            card.map(str::to_string),
        )
        .await
        {
            Err(e) => {
                warn!(
                    "[People] Failed to sync people profile for user {}: {}",
                    user_id, e
                );
            }
            Ok(()) => {
                state
                    .people_profile_paths
                    .write()
                    .await
                    .insert(user_id_str.clone(), profile_path.clone());
                let profile_name = profile_path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or(&user_id_str)
                    .to_string();
                if let Err(e) = state.set_profile(user_id_str.clone(), profile_name).await {
                    warn!("[People] Failed to persist profile cache: {}", e);
                }
            }
        }
        return;
    }

    // Cached DB mapping can be stale after manual cleanup or migration.
    match state.has_profile(&user_id_str).await {
        Ok(true) => {
            warn!(
                "[People] Profile cache exists for user {}, but file is missing; recreating",
                user_id
            );
        }
        Ok(false) => {}
        Err(e) => warn!(
            "[People] Failed to query profile cache for user {}: {}",
            user_id, e
        ),
    }

    // ── New profile: create with all available info ────────────────────
    // Priority: sender.nickname > sender.card > get_stranger_info > "QQ用户{id}"
    let mut nickname = sender
        .and_then(|s| s.nickname.clone())
        .filter(|n| !n.is_empty())
        .or_else(|| card.map(|c| c.to_string()))
        .unwrap_or_else(|| format!("QQ用户{}", user_id));

    match call_api(
        ws_tx,
        pending,
        "get_stranger_info",
        serde_json::json!({"user_id": user_id}),
        state.config.read().await.onebot_api_timeout_secs,
    )
    .await
    {
        Ok(resp) => {
            if let Some(data) = resp.data
                && let Some(n) = data.get("nickname").and_then(|n| n.as_str())
                && !n.is_empty()
            {
                nickname = n.to_string();
            }
        }
        Err(e) => {
            debug!("get_stranger_info failed, using sender info: {}", e);
        }
    }

    // Initialize group_cards with current group (if any)
    let mut group_cards: HashMap<String, GroupCardInfo> = HashMap::new();
    if let (Some(card), Some(gid)) = (card, &group_id_str) {
        group_cards.insert(
            gid.clone(),
            GroupCardInfo {
                card: card.to_string(),
            },
        );
    }

    let group_cards_len = group_cards.len();
    if let Err(e) =
        create_profile_file_async(profile_path.clone(), user_id, nickname.clone(), group_cards)
            .await
    {
        tracing::error!("Failed to create people profile: {}", e);
        return;
    }

    state
        .people_profile_paths
        .write()
        .await
        .insert(user_id_str.clone(), profile_path.clone());
    if let Err(e) = state.set_profile(user_id_str.clone(), user_id_str).await {
        warn!("[People] Failed to persist profile cache: {}", e);
    }
    info!(
        "People profile created: {} → {}.md (group_cards: {})",
        nickname, user_id, group_cards_len
    );
}

async fn resolve_profile_path_for_user(state: &SharedState, user_id: i64) -> PathBuf {
    let user_id = user_id.to_string();
    if let Some(path) = state
        .people_profile_paths
        .read()
        .await
        .get(&user_id)
        .cloned()
        && path.exists()
    {
        return path;
    }

    let people_dir = state.config.read().await.people_dir.clone();
    let fallback = people_dir.join(format!("{}.md", user_id));
    let people_dir_for_fallback = people_dir.clone();
    let user_id_for_scan = user_id.clone();
    let path = tokio::task::spawn_blocking(move || {
        find_profile_path_by_qq_id(&people_dir, &user_id_for_scan).unwrap_or(fallback)
    })
    .await
    .unwrap_or_else(|e| {
        warn!("[People] Profile path scan task failed: {}", e);
        people_dir_for_fallback.join(format!("{}.md", user_id))
    });

    state
        .people_profile_paths
        .write()
        .await
        .insert(user_id, path.clone());
    path
}

fn find_profile_path_by_qq_id(people_dir: &Path, qq_id: &str) -> Option<PathBuf> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(people_dir)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("md"))
        .collect();
    paths.sort();

    for path in paths {
        let content = match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(_) => continue,
        };
        let (frontmatter, _) = split_frontmatter(&content);
        let matched = extract_frontmatter_field_from_lines(&frontmatter, "qq_id")
            .map(|value| value == qq_id)
            .unwrap_or(false);
        if matched {
            return Some(path);
        }
    }

    None
}

fn create_profile_file(
    path: &PathBuf,
    user_id: i64,
    nickname: &str,
    group_cards: &HashMap<String, GroupCardInfo>,
) -> Result<(), std::io::Error> {
    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let today = today_string();
    let frontmatter = vec![
        format!("telegram_id: \"{}\"", user_id),
        format!("qq_id: \"{}\"", user_id),
        format!("username: \"{}\"", escape_frontmatter_value(nickname)),
    ];
    let body = build_default_body(nickname, &user_id.to_string(), &today, group_cards);
    let content = render_profile_content(&frontmatter, &body);

    std::fs::write(path, content)?;
    info!("Created people profile: {:?}", path);
    Ok(())
}

async fn create_profile_file_async(
    path: PathBuf,
    user_id: i64,
    nickname: String,
    group_cards: HashMap<String, GroupCardInfo>,
) -> Result<(), std::io::Error> {
    tokio::task::spawn_blocking(move || {
        create_profile_file(&path, user_id, &nickname, &group_cards)
    })
    .await
    .map_err(std::io::Error::other)?
}

/// Sync an existing profile file with bridge-managed metadata.
///
/// This keeps Alma-compatible frontmatter, preserves the user's freeform notes,
/// and stores per-group cards in a structured markdown section so the LLM can
/// distinguish the same person across different QQ groups.
fn sync_profile_file(
    path: &PathBuf,
    user_id: i64,
    preferred_name: Option<&str>,
    group_id: Option<&str>,
    card: Option<&str>,
) -> Result<(), std::io::Error> {
    let content = std::fs::read_to_string(path)?;
    let (mut frontmatter, body) = split_frontmatter(&content);

    let qq_id = extract_frontmatter_field_from_lines(&frontmatter, "qq_id")
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| user_id.to_string());
    let username = extract_frontmatter_field_from_lines(&frontmatter, "username")
        .filter(|v| !v.is_empty())
        .or_else(|| preferred_name.map(|s| s.to_string()))
        .unwrap_or_else(|| format!("QQ用户{}", qq_id));

    upsert_frontmatter_field(&mut frontmatter, "telegram_id", &qq_id);
    upsert_frontmatter_field(&mut frontmatter, "qq_id", &qq_id);
    upsert_frontmatter_field(&mut frontmatter, "username", &username);

    let mut group_cards = extract_group_cards_from_body(&body);
    if let (Some(gid), Some(group_card)) = (group_id, card.filter(|c| !c.is_empty())) {
        upsert_group_card(&mut group_cards, gid, group_card);
    }

    let mut cleaned_body = strip_group_cards_section(&body).trim().to_string();
    if cleaned_body.is_empty() {
        let first_interaction =
            extract_body_line(&content, "首次互动:").unwrap_or_else(today_string);
        cleaned_body = build_default_body(&username, &qq_id, &first_interaction, &group_cards);
    } else if !group_cards.is_empty() {
        cleaned_body.push_str("\n\n");
        cleaned_body.push_str(&format_group_cards_section(&group_cards));
    }

    let new_content = render_profile_content(&frontmatter, &cleaned_body);
    if new_content == content {
        return Ok(());
    }

    std::fs::write(path, new_content)?;
    info!(
        "[People] Synced people profile for user {} (group cards: {})",
        qq_id,
        group_cards.len()
    );
    Ok(())
}

async fn sync_profile_file_async(
    path: PathBuf,
    user_id: i64,
    preferred_name: Option<String>,
    group_id: Option<String>,
    card: Option<String>,
) -> Result<(), std::io::Error> {
    tokio::task::spawn_blocking(move || {
        sync_profile_file(
            &path,
            user_id,
            preferred_name.as_deref(),
            group_id.as_deref(),
            card.as_deref(),
        )
    })
    .await
    .map_err(std::io::Error::other)?
}

fn extract_group_cards_from_body(body: &str) -> HashMap<String, GroupCardInfo> {
    let mut cards = HashMap::new();
    let lines: Vec<&str> = body.lines().collect();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i].trim();

        if line == "- 群名片:" {
            i += 1;
            while i < lines.len() {
                let nested_raw = lines[i];
                let nested = nested_raw.trim();
                if nested.is_empty() {
                    i += 1;
                    continue;
                }
                if nested_raw.starts_with("  - ") {
                    if let Some((gid, card)) = parse_group_card_entry(nested) {
                        cards.insert(gid, GroupCardInfo { card });
                    }
                    i += 1;
                    continue;
                }
                break;
            }
            continue;
        }

        i += 1;
    }

    cards
}

fn parse_group_card_entry(entry: &str) -> Option<(String, String)> {
    let entry = entry.trim().strip_prefix("- ").unwrap_or(entry.trim());
    let (gid, card) = entry.split_once(':')?;
    let gid = gid.trim();
    let card = card.trim();
    if gid.is_empty() || card.is_empty() {
        return None;
    }
    Some((gid.to_string(), card.to_string()))
}

fn upsert_group_card(group_cards: &mut HashMap<String, GroupCardInfo>, group_id: &str, card: &str) {
    let entry = group_cards
        .entry(group_id.to_string())
        .or_insert_with(|| GroupCardInfo {
            card: card.to_string(),
        });
    entry.card = card.to_string();
}

/// Extract a single quoted field value from frontmatter lines, e.g.
/// `qq_id: "123456"` → `Some("123456")`.
fn extract_frontmatter_field_from_lines(lines: &[String], field: &str) -> Option<String> {
    let prefix = format!("{}:", field);
    let line = lines.iter().find(|l| l.starts_with(&prefix))?;
    let val = line.trim_start_matches(&prefix).trim().trim_matches('"');
    Some(val.to_string())
}

fn upsert_frontmatter_field(lines: &mut Vec<String>, field: &str, value: &str) {
    let prefix = format!("{}:", field);
    let rendered = format!("{}: \"{}\"", field, escape_frontmatter_value(value));
    if let Some(line) = lines.iter_mut().find(|line| line.starts_with(&prefix)) {
        *line = rendered;
    } else {
        lines.push(rendered);
    }
}

/// Extract a value from a body bullet line like `- 首次互动: 2026-06-20`.
fn extract_body_line(content: &str, label: &str) -> Option<String> {
    let line = content.lines().find(|l| l.contains(label))?;
    line.split(label).nth(1).map(|s| s.trim().to_string())
}

fn build_default_body(
    nickname: &str,
    qq_id: &str,
    first_interaction: &str,
    group_cards: &HashMap<String, GroupCardInfo>,
) -> String {
    let mut lines = vec![
        format!("# {}", nickname),
        String::new(),
        format!("- QQ 用户，ID: {}", qq_id),
        format!("- 昵称: {}", nickname),
        format!("- 首次互动: {}", first_interaction),
    ];
    if !group_cards.is_empty() {
        lines.push(String::new());
        lines.push(format_group_cards_section(group_cards));
    }
    lines.join("\n")
}

fn format_group_cards_section(group_cards: &HashMap<String, GroupCardInfo>) -> String {
    if group_cards.is_empty() {
        return String::new();
    }
    let mut entries: Vec<_> = group_cards.iter().collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    let mut lines = vec!["- 群名片:".to_string()];
    for (gid, info) in entries {
        lines.push(format!("  - {}: {}", gid, info.card));
    }
    lines.join("\n")
}

fn strip_group_cards_section(body: &str) -> String {
    let lines: Vec<&str> = body.lines().collect();
    let mut kept = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();

        if trimmed == "- 群名片:" {
            i += 1;
            while i < lines.len() {
                let nested_raw = lines[i];
                let nested = nested_raw.trim();
                if nested_raw.starts_with("  - ") || nested.is_empty() {
                    i += 1;
                    continue;
                }
                break;
            }
            continue;
        }

        kept.push(line);
        i += 1;
    }

    kept.join("\n").trim().to_string()
}

fn split_frontmatter(content: &str) -> (Vec<String>, String) {
    if !content.starts_with("---\n") {
        return (Vec::new(), content.to_string());
    }

    let mut frontmatter = Vec::new();
    let mut body_lines = Vec::new();
    let mut in_frontmatter = true;

    for (idx, line) in content.lines().enumerate() {
        if idx == 0 {
            continue;
        }
        if in_frontmatter && line == "---" {
            in_frontmatter = false;
            continue;
        }
        if in_frontmatter {
            frontmatter.push(line.to_string());
        } else {
            body_lines.push(line);
        }
    }

    (frontmatter, body_lines.join("\n"))
}

fn render_profile_content(frontmatter: &[String], body: &str) -> String {
    let mut content = String::from("---\n");
    if !frontmatter.is_empty() {
        content.push_str(&frontmatter.join("\n"));
        content.push('\n');
    }
    content.push_str("---\n");
    let trimmed_body = body.trim();
    if !trimmed_body.is_empty() {
        content.push_str(trimmed_body);
        content.push('\n');
    }
    content
}

fn escape_frontmatter_value(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Get today's date as YYYY-MM-DD string (UTC+8).
fn today_string() -> String {
    use time::{OffsetDateTime, UtcOffset, format_description};
    let utc = OffsetDateTime::now_utc();
    let cst = utc.to_offset(UtcOffset::from_hms(8, 0, 0).unwrap());
    let format = format_description::parse_borrowed::<2>("[year]-[month]-[day]").unwrap();
    cst.format(&format)
        .unwrap_or_else(|_| "unknown".to_string())
}

/// Find a SENDER PROFILE block for the given QQ user by scanning people/*.md files.
///
/// Matches by `qq_id` in YAML frontmatter, falls back to filename matching.
/// Returns `None` if no profile found or profile exceeds 500 chars.
pub async fn find_sender_profile(
    state: &SharedState,
    qq_id: &str,
    display_name: &str,
) -> Option<String> {
    let people_dir = state.config.read().await.people_dir.clone();
    let qq_id = qq_id.to_string();
    let display_name = display_name.to_string();
    let cached_path = state.people_profile_paths.read().await.get(&qq_id).cloned();
    let qq_id_for_scan = qq_id.clone();

    let result = tokio::task::spawn_blocking(move || {
        find_sender_profile_block(
            &people_dir,
            &qq_id_for_scan,
            &display_name,
            cached_path.as_deref(),
        )
    })
    .await
    .ok()
    .flatten();

    if let Some((path, block)) = result {
        state.people_profile_paths.write().await.insert(qq_id, path);
        Some(block)
    } else {
        None
    }
}

fn find_sender_profile_block(
    people_dir: &std::path::Path,
    qq_id: &str,
    display_name: &str,
    cached_path: Option<&Path>,
) -> Option<(PathBuf, String)> {
    if let Some(path) = cached_path
        && let Some(block) = profile_block_from_path(path, qq_id, display_name)
    {
        return Some((path.to_path_buf(), block));
    }

    let entries = match std::fs::read_dir(people_dir) {
        Ok(e) => e,
        Err(_) => return None,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }

        let Some(block) = profile_block_from_path(&path, qq_id, display_name) else {
            continue;
        };
        return Some((path, block));
    }

    None
}

fn profile_block_from_path(path: &Path, qq_id: &str, display_name: &str) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let (frontmatter, _) = split_frontmatter(&content);
    let matched = extract_frontmatter_field_from_lines(&frontmatter, "qq_id")
        .map(|value| value == qq_id)
        .unwrap_or(false);

    // Fallback: match by filename == display_name (case-insensitive)
    let matched = matched || {
        path.file_stem()
            .and_then(|s| s.to_str())
            .map(|stem| stem.eq_ignore_ascii_case(display_name))
            .unwrap_or(false)
    };

    if matched && content.len() < 500 {
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(display_name);

        return Some(format!(
            "\n\n[SENDER PROFILE — {}]:\n{}\n[/SENDER PROFILE]",
            name, content
        ));
    }

    None
}

/// Count total .md profile files in the people directory.
pub async fn count_profiles(state: &SharedState) -> usize {
    let people_dir = state.config.read().await.people_dir.clone();
    tokio::task::spawn_blocking(move || count_profiles_in_dir(&people_dir))
        .await
        .unwrap_or(0)
}

fn count_profiles_in_dir(people_dir: &std::path::Path) -> usize {
    std::fs::read_dir(people_dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.path().extension().and_then(|ext| ext.to_str()) == Some("md"))
                .count()
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{
        GroupCardInfo, build_default_body, create_profile_file, extract_group_cards_from_body,
        find_profile_path_by_qq_id, find_sender_profile_block, format_group_cards_section,
        sync_profile_file,
    };
    use std::collections::HashMap;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_profile_path(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("alma-onebot-bridge-{name}-{nonce}.md"))
    }

    fn temp_profile_dir(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("alma-onebot-bridge-{name}-{nonce}"))
    }

    #[test]
    fn create_profile_file_writes_structured_group_cards_section() {
        let path = temp_profile_path("create");
        let mut group_cards = HashMap::new();
        group_cards.insert(
            "123".to_string(),
            GroupCardInfo {
                card: "群名片A".to_string(),
            },
        );

        create_profile_file(&path, 42, "Alice", &group_cards).unwrap();
        let content = fs::read_to_string(&path).unwrap();
        let _ = fs::remove_file(&path);

        assert!(content.contains("telegram_id: \"42\""));
        assert!(content.contains("qq_id: \"42\""));
        assert!(!content.contains("group_cards:"));
        assert!(content.contains("- 群名片:\n  - 123: 群名片A"));
    }

    #[test]
    fn resolve_profile_path_prefers_existing_qq_id_profile() {
        let dir = temp_profile_dir("people-dir");
        fs::create_dir_all(&dir).unwrap();
        let existing = dir.join("Alice.md");
        fs::write(
            &existing,
            r#"---
qq_id: "42"
username: "Alice"
---
# Alice
"#,
        )
        .unwrap();

        let resolved = find_profile_path_by_qq_id(&dir, "42").unwrap_or_else(|| dir.join("42.md"));
        let fallback = dir.join("42.md");
        let _ = fs::remove_dir_all(&dir);

        assert_eq!(resolved, existing);
        assert_ne!(resolved, fallback);
    }

    #[test]
    fn find_sender_profile_matches_qq_id_exactly() {
        let dir = temp_profile_dir("sender-profile");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("Wrong.md"),
            r#"---
qq_id: "12345"
username: "Wrong"
---
# Wrong
"#,
        )
        .unwrap();
        fs::write(
            dir.join("Right.md"),
            r#"---
qq_id: "123"
username: "Right"
---
# Right
"#,
        )
        .unwrap();

        let (_path, profile) = find_sender_profile_block(&dir, "123", "Someone", None).unwrap();
        let _ = fs::remove_dir_all(&dir);

        assert!(profile.contains("SENDER PROFILE — Right"));
        assert!(!profile.contains("SENDER PROFILE — Wrong"));
    }

    #[test]
    fn sync_profile_file_updates_structured_group_cards_section() {
        let path = temp_profile_path("sync");
        let existing = r#"---
telegram_id: "42"
qq_id: "42"
username: "Alice"
---
# Alice

- QQ 用户，ID: 42
- 昵称: Alice
- 首次互动: 2026-06-20
- 备注: 已有内容
- 群名片:
  - 100: 一群名片
"#;
        fs::write(&path, existing).unwrap();

        sync_profile_file(&path, 42, Some("Alice"), Some("200"), Some("二群名片")).unwrap();
        let content = fs::read_to_string(&path).unwrap();
        let _ = fs::remove_file(&path);

        assert!(content.contains("- 备注: 已有内容"));
        assert!(content.contains("- 群名片:\n  - 100: 一群名片\n  - 200: 二群名片"));
    }

    #[test]
    fn extract_group_cards_from_body_reads_structured_section() {
        let body = r#"# Alice

- QQ 用户，ID: 42
- 群名片:
  - 100: 旧名片
  - 200: 新名片
"#;

        let cards = extract_group_cards_from_body(body);
        assert_eq!(
            cards.get("100").map(|info| info.card.as_str()),
            Some("旧名片")
        );
        assert_eq!(
            cards.get("200").map(|info| info.card.as_str()),
            Some("新名片")
        );
    }

    #[test]
    fn format_group_cards_section_is_structured_and_stable() {
        let mut group_cards = HashMap::new();
        group_cards.insert(
            "200".to_string(),
            GroupCardInfo {
                card: "B".to_string(),
            },
        );
        group_cards.insert(
            "100".to_string(),
            GroupCardInfo {
                card: "A".to_string(),
            },
        );

        assert_eq!(
            format_group_cards_section(&group_cards),
            "- 群名片:\n  - 100: A\n  - 200: B"
        );
    }

    #[test]
    fn build_default_body_appends_group_cards_section() {
        let mut group_cards = HashMap::new();
        group_cards.insert(
            "100".to_string(),
            GroupCardInfo {
                card: "A".to_string(),
            },
        );

        let body = build_default_body("Alice", "42", "2026-06-20", &group_cards);
        assert!(body.contains("# Alice"));
        assert!(body.contains("- 首次互动: 2026-06-20"));
        assert!(body.contains("- 群名片:\n  - 100: A"));
    }
}
