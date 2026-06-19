use std::path::PathBuf;

use tracing::{debug, info};

use crate::onebot::event::Sender;
use crate::onebot::{PendingCalls, call_api};
use crate::state::SharedState;
use tokio::sync::mpsc;
use warp::ws::Message;

/// Ensure a People Profile exists for the given QQ user.
/// If the profile doesn't exist, fetch user info from OneBot and create it.
pub async fn ensure_people_profile(
    user_id: i64,
    sender: Option<&Sender>,
    state: &SharedState,
    ws_tx: &mpsc::UnboundedSender<Message>,
    pending: &PendingCalls,
) {
    let user_id_str = user_id.to_string();

    if state.has_profile(&user_id_str).await {
        return;
    }

    // Try to get detailed user info from OneBot
    let mut nickname = sender
        .and_then(|s| s.nickname.clone())
        .unwrap_or_else(|| format!("QQ用户{}", user_id));

    match call_api(
        ws_tx,
        pending,
        "get_stranger_info",
        serde_json::json!({"user_id": user_id}),
        state.config.onebot_api_timeout_secs,
    )
    .await
    {
        Ok(resp) => {
            if let Some(data) = resp.data {
                if let Some(n) = data.get("nickname").and_then(|n| n.as_str()) {
                    nickname = n.to_string();
                }
            }
        }
        Err(e) => {
            debug!("get_stranger_info failed, using sender info: {}", e);
        }
    }

    // Use QQ ID as filename to avoid collisions when different users
    // share the same nickname across groups.
    let profile_path = state.config.people_dir.join(format!("{}.md", user_id));

    if !profile_path.exists() {
        if let Err(e) = create_profile_file(&profile_path, user_id, &nickname) {
            tracing::error!("Failed to create people profile: {}", e);
            return;
        }
    }

    state.set_profile(user_id_str.clone(), user_id_str).await;
    info!("People profile ensured: {} → {}.md", nickname, user_id);
}

fn create_profile_file(path: &PathBuf, user_id: i64, nickname: &str) -> Result<(), std::io::Error> {
    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let today = today_string();
    let content = format!(
        "---\ntelegram_id: \"{user_id}\"\nqq_id: \"{user_id}\"\nusername: \"{nickname}\"\n---\n\
         # {nickname}\n\n\
         - QQ 用户，ID: {user_id}\n\
         - 昵称: {nickname}\n\
         - 首次互动: {today}\n",
    );

    std::fs::write(path, content)?;
    info!("Created people profile: {:?}", path);
    Ok(())
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
pub fn find_sender_profile(
    people_dir: &std::path::Path,
    qq_id: &str,
    display_name: &str,
) -> Option<String> {
    let entries = match std::fs::read_dir(people_dir) {
        Ok(e) => e,
        Err(_) => return None,
    };

    let qq_id_quoted = format!("qq_id: \"{}\"", qq_id);
    let qq_id_plain = format!("qq_id: {}", qq_id);

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }

        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        // Match by qq_id in frontmatter
        let matched = content.contains(&qq_id_quoted) || content.contains(&qq_id_plain);

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
    }

    None
}

/// Count total .md profile files in the people directory.
pub fn count_profiles(people_dir: &std::path::Path) -> usize {
    std::fs::read_dir(people_dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.path().extension().and_then(|ext| ext.to_str()) == Some("md"))
                .count()
        })
        .unwrap_or(0)
}
