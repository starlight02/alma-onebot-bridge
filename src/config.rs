use std::path::PathBuf;

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

/// Bridge configuration — populated from config.toml with built-in defaults.
#[derive(Clone, Debug)]
pub struct Config {
    pub bridge_port: u16,
    pub alma_api: String,
    pub people_dir: PathBuf,
    pub db_path: PathBuf,
    /// Preferred bootstrap model for new threads, or fallback when thread model lookup fails.
    /// Priority: config.toml alma.model > Alma settings API.
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

impl Config {
    pub fn load() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));

        // Try to load config.toml
        let file_config = Self::load_config_file();

        // Extract TOML section values
        let bridge = file_config.bridge.unwrap_or_default();
        let alma = file_config.alma.unwrap_or_default();
        let database = file_config.database.unwrap_or_default();
        let people = file_config.people.unwrap_or_default();
        let onebot = file_config.onebot.unwrap_or_default();
        let chat = file_config.chat.unwrap_or_default();

        Config {
            bridge_port: bridge.port.unwrap_or(8090),
            alma_api: alma
                .api
                .unwrap_or_else(|| "http://localhost:23001".to_string()),
            people_dir: people
                .dir
                .unwrap_or_else(|| {
                    home.join(".config/alma/people")
                        .to_string_lossy()
                        .to_string()
                })
                .into(),
            db_path: database
                .path
                .unwrap_or_else(|| "bridge-state.db".to_string())
                .into(),
            alma_model: alma.model,
            alma_run_timeout_secs: alma.timeout.unwrap_or(120),
            alma_max_retries: alma.max_retries.unwrap_or(2),
            alma_retry_delay_ms: alma.retry_delay_ms.unwrap_or(3000),
            onebot_api_timeout_secs: onebot.api_timeout.unwrap_or(30),
            access_token: onebot
                .access_token
                .map(|token| token.trim().to_string())
                .filter(|token| !token.is_empty()),
            group_history_size: chat.group_history_size.unwrap_or(30),
            thinking_message: chat.thinking_message,
            show_thinking: chat.show_thinking.unwrap_or(false),
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

        tracing::info!("No config.toml found, using defaults");
        FileConfig::default()
    }
}
