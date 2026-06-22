use std::env;
use std::path::PathBuf;
use std::str::FromStr;

use serde::Deserialize;

/// TOML config file structure.
#[derive(Deserialize, Default)]
struct FileConfig {
    bridge: Option<BridgeSection>,
    alma: Option<AlmaSection>,
    database: Option<DatabaseSection>,
    people: Option<PeopleSection>,
    onebot: Option<OneBotSection>,
    chat: Option<ChatSection>,
}

#[derive(Deserialize, Default)]
struct BridgeSection {
    port: Option<u16>,
}

#[derive(Deserialize, Default)]
struct AlmaSection {
    api: Option<String>,
    model: Option<String>,
    timeout: Option<u64>,
    max_retries: Option<u32>,
    retry_delay_ms: Option<u64>,
}

#[derive(Deserialize, Default)]
struct DatabaseSection {
    path: Option<String>,
}

#[derive(Deserialize, Default)]
struct PeopleSection {
    dir: Option<String>,
}

#[derive(Deserialize, Default)]
struct OneBotSection {
    api_timeout: Option<u64>,
    access_token: Option<String>,
}

#[derive(Deserialize, Default)]
struct ChatSection {
    group_history_size: Option<usize>,
    thinking_message: Option<String>,
    show_thinking: Option<bool>,
}

/// Bridge configuration — populated from config.toml with env var overrides.
#[derive(Clone, Debug)]
pub struct Config {
    pub bridge_port: u16,
    pub alma_api: String,
    pub people_dir: PathBuf,
    pub db_path: PathBuf,
    /// Preferred bootstrap model for new threads, or fallback when thread model lookup fails.
    /// Priority: ALMA_MODEL env var > config.toml alma.model > Alma settings API.
    pub alma_model: Option<String>,
    pub alma_run_timeout_secs: u64,
    pub alma_max_retries: u32,
    pub alma_retry_delay_ms: u64,
    pub onebot_api_timeout_secs: u64,
    pub access_token: Option<String>,
    /// Number of recent group messages to keep in memory for context injection.
    /// Set to 0 to disable group history. Default: 30.
    pub group_history_size: usize,
    /// Optional "thinking..." message sent before generation starts.
    /// None = disabled (default). Some("思考中...") = enabled.
    pub thinking_message: Option<String>,
    /// Whether to show AI thinking content (from `<think>` / `<thinking>` blocks)
    /// as a separate message before the main reply. Default: false (strip silently).
    pub show_thinking: bool,
}

fn get_env_or<T>(env_key: &str, toml_val: Option<T>, default: T) -> T
where
    T: FromStr,
{
    match env::var(env_key) {
        Ok(value) => match value.trim().parse::<T>() {
            Ok(parsed) => parsed,
            Err(_) => {
                tracing::warn!(
                    "Invalid {}='{}'; falling back to config/default",
                    env_key,
                    value
                );
                toml_val.unwrap_or(default)
            }
        },
        Err(_) => toml_val.unwrap_or(default),
    }
}

fn get_env_or_bool(env_key: &str, toml_val: Option<bool>, default: bool) -> bool {
    match env::var(env_key) {
        Ok(value) => match parse_bool_value(&value) {
            Some(parsed) => parsed,
            None => {
                tracing::warn!(
                    "Invalid {}='{}'; expected true/false/1/0/on/off/yes/no, falling back to config/default",
                    env_key,
                    value
                );
                toml_val.unwrap_or(default)
            }
        },
        Err(_) => toml_val.unwrap_or(default),
    }
}

fn parse_bool_value(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "y" | "on" => Some(true),
        "false" | "0" | "no" | "n" | "off" => Some(false),
        _ => None,
    }
}

impl Config {
    pub fn from_env() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));

        // Try to load config.toml
        let file_config = Self::load_config_file();

        let get_string = |env_key: &str, toml_val: Option<String>, default: String| -> String {
            env::var(env_key).ok().or(toml_val).unwrap_or(default)
        };

        let get_opt_string = |env_key: &str, toml_val: Option<String>| -> Option<String> {
            env::var(env_key).ok().or(toml_val)
        };

        // Extract TOML section values
        let bridge = file_config.bridge.unwrap_or_default();
        let alma = file_config.alma.unwrap_or_default();
        let database = file_config.database.unwrap_or_default();
        let people = file_config.people.unwrap_or_default();
        let onebot = file_config.onebot.unwrap_or_default();
        let chat = file_config.chat.unwrap_or_default();

        Config {
            bridge_port: get_env_or("BRIDGE_PORT", bridge.port, 8090),
            alma_api: get_string("ALMA_API", alma.api, "http://localhost:23001".to_string()),
            people_dir: get_string(
                "PEOPLE_DIR",
                people.dir,
                home.join(".config/alma/people")
                    .to_string_lossy()
                    .to_string(),
            )
            .into(),
            db_path: get_string("DB_PATH", database.path, "bridge-state.db".to_string()).into(),
            alma_model: get_opt_string("ALMA_MODEL", alma.model),
            alma_run_timeout_secs: get_env_or("ALMA_TIMEOUT", alma.timeout, 120),
            alma_max_retries: get_env_or("ALMA_MAX_RETRIES", alma.max_retries, 2),
            alma_retry_delay_ms: get_env_or("ALMA_RETRY_DELAY", alma.retry_delay_ms, 3000),
            onebot_api_timeout_secs: get_env_or("ONEBOT_API_TIMEOUT", onebot.api_timeout, 30),
            access_token: get_opt_string("ACCESS_TOKEN", onebot.access_token)
                .map(|token| token.trim().to_string())
                .filter(|token| !token.is_empty()),
            group_history_size: get_env_or("GROUP_HISTORY_SIZE", chat.group_history_size, 30),
            thinking_message: get_opt_string("THINKING_MESSAGE", chat.thinking_message),
            show_thinking: get_env_or_bool("SHOW_THINKING", chat.show_thinking, false),
        }
    }

    fn load_config_file() -> FileConfig {
        // Try multiple locations for config.toml
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let candidates = [
            PathBuf::from("config.toml"),
            PathBuf::from("bridge.toml"),
            home.join(".config/alma/bridge/config.toml"),
        ];

        for path in &candidates {
            if let Ok(content) = std::fs::read_to_string(path) {
                match toml::from_str::<FileConfig>(&content) {
                    Ok(config) => {
                        tracing::info!("Loaded config from {:?}", path);
                        return config;
                    }
                    Err(e) => {
                        tracing::warn!("Failed to parse {:?}: {}", path, e);
                    }
                }
            }
        }

        tracing::info!("No config.toml found, using defaults + env vars");
        FileConfig::default()
    }
}

#[cfg(test)]
mod tests {
    use super::parse_bool_value;

    #[test]
    fn parse_bool_value_accepts_common_env_forms() {
        assert_eq!(parse_bool_value("true"), Some(true));
        assert_eq!(parse_bool_value("1"), Some(true));
        assert_eq!(parse_bool_value("ON"), Some(true));
        assert_eq!(parse_bool_value("false"), Some(false));
        assert_eq!(parse_bool_value("0"), Some(false));
        assert_eq!(parse_bool_value("off"), Some(false));
        assert_eq!(parse_bool_value("maybe"), None);
    }
}
