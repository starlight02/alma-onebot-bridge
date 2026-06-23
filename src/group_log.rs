use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::state::GroupDirectoryEntry;

const BRIDGE_README_START: &str = "<!-- alma-onebot-bridge:start -->";
const BRIDGE_README_END: &str = "<!-- alma-onebot-bridge:end -->";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GroupLogStats {
    pub days: usize,
    pub latest_file: Option<String>,
}

pub fn alma_groups_dir_at(home: &Path) -> PathBuf {
    home.join(".config/alma/groups")
}

pub fn alma_groups_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|home| alma_groups_dir_at(&home))
}

#[allow(clippy::too_many_arguments)]
pub fn append_alma_group_log(
    chat_id: i64,
    display_name: &str,
    text: &str,
    is_alma: bool,
    timestamp_secs: u64,
    message_id: Option<i64>,
    user_id: Option<i64>,
    username: Option<&str>,
) -> Result<(), std::io::Error> {
    let Some(home) = dirs::home_dir() else {
        return Ok(());
    };
    append_alma_group_log_at(
        &home,
        chat_id,
        display_name,
        text,
        is_alma,
        timestamp_secs,
        message_id,
        user_id,
        username,
    )
}

#[allow(clippy::too_many_arguments)]
pub async fn append_alma_group_log_async(
    chat_id: i64,
    display_name: String,
    text: String,
    is_alma: bool,
    timestamp_secs: u64,
    message_id: Option<i64>,
    user_id: Option<i64>,
    username: Option<String>,
) -> Result<(), std::io::Error> {
    tokio::task::spawn_blocking(move || {
        append_alma_group_log(
            chat_id,
            &display_name,
            &text,
            is_alma,
            timestamp_secs,
            message_id,
            user_id,
            username.as_deref(),
        )
    })
    .await
    .map_err(std::io::Error::other)?
}

#[allow(clippy::too_many_arguments)]
pub fn append_alma_group_log_at(
    home: &Path,
    chat_id: i64,
    display_name: &str,
    text: &str,
    is_alma: bool,
    timestamp_secs: u64,
    message_id: Option<i64>,
    user_id: Option<i64>,
    username: Option<&str>,
) -> Result<(), std::io::Error> {
    let dir = alma_groups_dir_at(home);
    std::fs::create_dir_all(&dir)?;

    let instant = local_time(timestamp_secs);
    let date = format!(
        "{:04}-{:02}-{:02}",
        instant.year(),
        u8::from(instant.month()),
        instant.day()
    );
    let clock = format!(
        "{:02}:{:02}:{:02}",
        instant.hour(),
        instant.minute(),
        instant.second()
    );

    let path = dir.join(format!("{}_{}.log", chat_id, date));
    let msg = message_id
        .map(|id| format!(" [msg:{}]", id))
        .unwrap_or_default();
    let sender = if is_alma {
        "[Alma (BOT)]".to_string()
    } else if let Some(uid) = user_id {
        match username {
            Some(name) if !name.is_empty() => {
                format!("[{} (@{}) [id:{}]]", display_name, name, uid)
            }
            _ => format!("[{} [id:{}]]", display_name, uid),
        }
    } else {
        format!("[{}]", display_name)
    };
    let one_line = text.replace(['\r', '\n'], " ");
    let line = format!("[{}]{} {}: {}\n", clock, msg, sender, one_line);

    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(line.as_bytes())
}

pub fn write_group_readme(
    entries: &[GroupDirectoryEntry],
    bridge_port: u16,
) -> Result<(), std::io::Error> {
    let Some(home) = dirs::home_dir() else {
        return Ok(());
    };
    write_group_readme_at(&home, entries, bridge_port)
}

pub async fn write_group_readme_async(
    entries: Vec<GroupDirectoryEntry>,
    bridge_port: u16,
) -> Result<(), std::io::Error> {
    tokio::task::spawn_blocking(move || write_group_readme(&entries, bridge_port))
        .await
        .map_err(std::io::Error::other)?
}

pub fn write_group_readme_at(
    home: &Path,
    entries: &[GroupDirectoryEntry],
    bridge_port: u16,
) -> Result<(), std::io::Error> {
    let dir = alma_groups_dir_at(home);
    std::fs::create_dir_all(&dir)?;
    let stats = collect_group_log_stats_at(home)?;
    let bridge_section = build_group_readme(entries, &stats, bridge_port);
    let path = dir.join("README.md");
    let existing = match std::fs::read_to_string(&path) {
        Ok(content) => Some(content),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    let readme = merge_group_readme(existing.as_deref(), &bridge_section);
    if existing.as_deref() == Some(readme.as_str()) {
        return Ok(());
    }
    std::fs::write(path, readme)
}

pub fn collect_group_log_stats_at(
    home: &Path,
) -> Result<BTreeMap<i64, GroupLogStats>, std::io::Error> {
    let dir = alma_groups_dir_at(home);
    let mut stats = BTreeMap::<i64, GroupLogStats>::new();
    if !dir.exists() {
        return Ok(stats);
    }

    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let Some(file_name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if !file_name.ends_with(".log") {
            continue;
        }
        let Some((chat_id, _date)) = file_name.split_once('_') else {
            continue;
        };
        let Ok(chat_id) = chat_id.parse::<i64>() else {
            continue;
        };

        let group_stats = stats.entry(chat_id).or_default();
        group_stats.days += 1;
        if group_stats
            .latest_file
            .as_ref()
            .map(|latest| file_name > *latest)
            .unwrap_or(true)
        {
            group_stats.latest_file = Some(file_name);
        }
    }

    Ok(stats)
}

pub fn build_group_readme(
    entries: &[GroupDirectoryEntry],
    log_stats: &BTreeMap<i64, GroupLogStats>,
    bridge_port: u16,
) -> String {
    let body = build_group_readme_body(entries, log_stats, bridge_port);
    format!("{BRIDGE_README_START}\n{body}{BRIDGE_README_END}\n")
}

fn build_group_readme_body(
    entries: &[GroupDirectoryEntry],
    log_stats: &BTreeMap<i64, GroupLogStats>,
    bridge_port: u16,
) -> String {
    let mut groups = BTreeMap::<i64, GroupDirectoryEntry>::new();
    for entry in entries {
        groups.insert(entry.group_id, entry.clone());
    }
    for group_id in log_stats.keys() {
        groups
            .entry(*group_id)
            .or_insert_with(|| GroupDirectoryEntry::unknown(*group_id));
    }

    let mut out = String::new();
    out.push_str("# QQ Groups\n\n");
    out.push_str("Auto-generated by alma-onebot-bridge. Alma's `alma group list`, `alma group history`, `alma group search`, and local-log part of `alma group context` read this directory.\n\n");
    out.push_str("For QQ / OneBot groups, `alma group send` is Telegram-only. To actively send a QQ group message, use the bridge endpoint from Alma tools:\n\n");
    out.push_str(&format!(
        "```bash\ncurl -s -X POST http://127.0.0.1:{}/qq/group/<chatId>/send \\\n  -H 'Content-Type: application/json' \\\n  -d '{{\"message\":\"...\"}}'\n```\n\n",
        bridge_port
    ));
    out.push_str("If the bridge is exposed beyond loopback, set `onebot.access_token` in config.toml; loopback requests are accepted so Alma can call the endpoint locally without leaking the token into prompts.\n\n");

    if groups.is_empty() {
        out.push_str("No QQ group logs yet.\n");
        return out;
    }

    out.push_str("## Groups\n\n");
    for (group_id, entry) in groups {
        let title = entry
            .title
            .as_deref()
            .filter(|title| !title.trim().is_empty())
            .map(str::trim)
            .unwrap_or("Unknown QQ Group");
        let stats = log_stats.get(&group_id).cloned().unwrap_or_default();

        out.push_str(&format!("### {}\n", title));
        out.push_str(&format!("- Chat ID: `{}`\n", group_id));
        out.push_str("- Platform: QQ / OneBot\n");
        out.push_str("- Type: group\n");
        out.push_str(&format!("- Log files: {} day(s)\n", stats.days));
        if let Some(latest) = stats.latest_file {
            out.push_str(&format!("- Latest log: `{}`\n", latest));
        }
        out.push_str(&format!(
            "- Last active: {}\n",
            format_timestamp_for_readme(entry.last_active)
        ));
        out.push_str(&format!(
            "- View history: `alma group history {} 100`\n",
            group_id
        ));
        out.push_str("- Search logs: `alma group search <keyword>`\n");
        out.push_str(&format!(
            "- Send message: `curl -s -X POST http://127.0.0.1:{}/qq/group/{}/send -H 'Content-Type: application/json' -d '{{\"message\":\"...\"}}'`\n",
            bridge_port, group_id
        ));
        out.push('\n');
    }

    out
}

fn merge_group_readme(existing: Option<&str>, bridge_section: &str) -> String {
    let bridge_section = ensure_trailing_newline(bridge_section);
    let Some(existing) = existing
        .map(str::trim)
        .filter(|content| !content.is_empty())
    else {
        return default_group_readme(&bridge_section);
    };

    if legacy_bridge_readme(existing) {
        return default_group_readme(&bridge_section);
    }

    if let Some(merged) = replace_bridge_section(existing, &bridge_section) {
        return merged;
    }

    format!("{}\n\n{}", existing.trim_end(), bridge_section)
}

fn default_group_readme(bridge_section: &str) -> String {
    format!(
        "# Alma Group Logs\n\nThis README may contain group directory sections from Alma and bridge integrations. alma-onebot-bridge only updates the marked section below.\n\n{}",
        bridge_section
    )
}

fn replace_bridge_section(existing: &str, bridge_section: &str) -> Option<String> {
    let start = existing.find(BRIDGE_README_START)?;
    let search_from = start + BRIDGE_README_START.len();
    let relative_end = existing[search_from..].find(BRIDGE_README_END)?;
    let end = search_from + relative_end + BRIDGE_README_END.len();

    let mut out = String::new();
    out.push_str(&existing[..start]);
    out.push_str(bridge_section);
    out.push_str(existing[end..].trim_start_matches('\n'));
    if !out.ends_with('\n') {
        out.push('\n');
    }
    Some(out)
}

fn legacy_bridge_readme(existing: &str) -> bool {
    !existing.contains(BRIDGE_README_START)
        && existing.starts_with("# QQ Groups")
        && existing.contains("alma-onebot-bridge")
}

fn ensure_trailing_newline(text: &str) -> String {
    let mut out = text.trim_end().to_string();
    out.push('\n');
    out
}

fn format_timestamp_for_readme(timestamp_secs: u64) -> String {
    if timestamp_secs == 0 {
        return "unknown".to_string();
    }
    let instant = local_time(timestamp_secs);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        instant.year(),
        u8::from(instant.month()),
        instant.day(),
        instant.hour(),
        instant.minute(),
        instant.second()
    )
}

fn local_time(timestamp_secs: u64) -> time::OffsetDateTime {
    timestamp_or_now(timestamp_secs).to_offset(local_offset())
}

fn local_offset() -> time::UtcOffset {
    time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC)
}

fn timestamp_or_now(timestamp_secs: u64) -> time::OffsetDateTime {
    if timestamp_secs == 0 {
        return time::OffsetDateTime::now_utc();
    }
    time::OffsetDateTime::from_unix_timestamp(timestamp_secs as i64)
        .unwrap_or_else(|_| time::OffsetDateTime::now_utc())
}

#[cfg(test)]
mod tests {
    use super::{
        GroupLogStats, append_alma_group_log_at, build_group_readme, collect_group_log_stats_at,
        merge_group_readme,
    };
    use crate::state::{GroupDirectoryEntry, GroupMember};
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_home(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("alma-onebot-bridge-{name}-{nonce}"))
    }

    #[test]
    fn group_log_writes_alma_group_directory_format() {
        let home = temp_home("group-log");
        append_alma_group_log_at(
            &home,
            706968284,
            "萌依",
            "你好\n世界",
            false,
            1_718_000_000,
            Some(42),
            Some(1757176294),
            None,
        )
        .unwrap();

        let stats = collect_group_log_stats_at(&home).unwrap();
        assert_eq!(stats.get(&706968284).unwrap().days, 1);

        let latest = stats
            .get(&706968284)
            .and_then(|s| s.latest_file.as_ref())
            .unwrap();
        let content =
            std::fs::read_to_string(home.join(".config/alma/groups").join(latest)).unwrap();

        assert!(content.contains("[msg:42] [萌依 [id:1757176294]]: 你好 世界"));
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn readme_includes_history_and_qq_send_endpoint_without_members() {
        let entries = vec![GroupDirectoryEntry {
            group_id: 706968284,
            title: Some("测试群".to_string()),
            last_active: 1_718_000_000,
            members: vec![GroupMember {
                group_id: 706968284,
                user_id: 1757176294,
                display_name: "萌依".to_string(),
                last_seen: 1_718_000_000,
            }],
        }];
        let mut stats = BTreeMap::new();
        stats.insert(
            706968284,
            GroupLogStats {
                days: 2,
                latest_file: Some("706968284_2026-06-21.log".to_string()),
            },
        );

        let readme = build_group_readme(&entries, &stats, 8090);

        assert!(readme.contains("### 测试群"));
        assert!(readme.contains("- Chat ID: `706968284`"));
        assert!(readme.contains("alma group history 706968284 100"));
        assert!(readme.contains("http://127.0.0.1:8090/qq/group/706968284/send"));
        assert!(!readme.contains("Known members"));
        assert!(!readme.contains("萌依 [id:1757176294]"));
        assert!(readme.contains("<!-- alma-onebot-bridge:start -->"));
        assert!(readme.contains("<!-- alma-onebot-bridge:end -->"));
    }

    #[test]
    fn readme_merge_preserves_existing_non_bridge_content() {
        let existing = "# Alma Groups\n\nTelegram group directory stays here.\n";
        let bridge_section = "<!-- alma-onebot-bridge:start -->\n## QQ Groups\n\n- Chat ID: `706968284`\n<!-- alma-onebot-bridge:end -->\n";

        let merged = merge_group_readme(Some(existing), bridge_section);

        assert!(merged.contains("Telegram group directory stays here."));
        assert!(merged.contains("## QQ Groups"));
        assert!(merged.contains("<!-- alma-onebot-bridge:start -->"));
        assert!(merged.contains("<!-- alma-onebot-bridge:end -->"));
    }

    #[test]
    fn readme_merge_replaces_only_existing_bridge_section() {
        let existing = "# Alma Groups\n\nBefore\n\n<!-- alma-onebot-bridge:start -->\nold bridge content\n<!-- alma-onebot-bridge:end -->\n\nAfter\n";
        let bridge_section = "<!-- alma-onebot-bridge:start -->\nnew bridge content\n<!-- alma-onebot-bridge:end -->\n";

        let merged = merge_group_readme(Some(existing), bridge_section);

        assert!(merged.contains("Before"));
        assert!(merged.contains("After"));
        assert!(merged.contains("new bridge content"));
        assert!(!merged.contains("old bridge content"));
    }

    #[test]
    fn readme_merge_replaces_legacy_bridge_owned_file() {
        let existing =
            "# QQ Groups\n\nAuto-generated by alma-onebot-bridge.\n\n## Groups\n\nold content\n";
        let bridge_section = "<!-- alma-onebot-bridge:start -->\nnew bridge content\n<!-- alma-onebot-bridge:end -->\n";

        let merged = merge_group_readme(Some(existing), bridge_section);

        assert!(merged.starts_with("# Alma Group Logs"));
        assert!(merged.contains("new bridge content"));
        assert!(!merged.contains("old content"));
    }
}
