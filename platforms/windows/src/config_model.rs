use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq)]
pub struct ConfigModel {
    pub bridge_port: String,
    pub alma_api: String,
    pub alma_model: String,
    pub alma_timeout: String,
    pub alma_max_retries: String,
    pub alma_retry_delay_ms: String,
    pub onebot_api_timeout: String,
    pub access_token: String,
    pub group_history_size: String,
    pub thinking_message: String,
    pub show_thinking: bool,
    pub show_tool_calls: bool,
    pub segmented_replies: bool,
    pub people_dir: String,
    pub db_path: String,
}

impl Default for ConfigModel {
    fn default() -> Self {
        let people_dir = dirs::home_dir()
            .map(|home| {
                home.join(".config/alma/people")
                    .to_string_lossy()
                    .to_string()
            })
            .unwrap_or_else(|| ".config/alma/people".to_string());

        Self {
            bridge_port: "8090".to_string(),
            alma_api: "http://localhost:23001".to_string(),
            alma_model: String::new(),
            alma_timeout: "120".to_string(),
            alma_max_retries: "2".to_string(),
            alma_retry_delay_ms: "3000".to_string(),
            onebot_api_timeout: "30".to_string(),
            access_token: String::new(),
            group_history_size: "30".to_string(),
            thinking_message: String::new(),
            show_thinking: false,
            show_tool_calls: false,
            segmented_replies: false,
            people_dir,
            db_path: "bridge-state.db".to_string(),
        }
    }
}

impl ConfigModel {
    pub fn load_from(path: &Path) -> Result<Self, String> {
        let content = fs::read_to_string(path).map_err(|e| e.to_string())?;
        let file_config: FileConfig = toml::from_str(&content).map_err(|e| e.to_string())?;
        Ok(Self::from_file(file_config))
    }

    pub fn save_to(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let file_config = self.to_file();
        let content = toml::to_string_pretty(&file_config).map_err(|e| e.to_string())?;
        fs::write(path, content).map_err(|e| e.to_string())
    }

    pub fn is_valid(&self) -> bool {
        self.bridge_port
            .parse::<u16>()
            .map(|port| port > 0)
            .unwrap_or(false)
            && self.is_http_url(&self.alma_api)
            && self.in_range(&self.alma_timeout, 1, 3600)
            && self.in_range(&self.alma_max_retries, 0, 10)
            && self.in_range(&self.alma_retry_delay_ms, 0, 600_000)
            && self.in_range(&self.onebot_api_timeout, 1, 600)
            && self.group_history_size.parse::<usize>().is_ok()
    }

    fn is_http_url(&self, value: &str) -> bool {
        value.starts_with("http://") || value.starts_with("https://")
    }

    fn in_range(&self, value: &str, min: u64, max: u64) -> bool {
        value
            .parse::<u64>()
            .map(|n| (min..=max).contains(&n))
            .unwrap_or(false)
    }

    fn from_file(file: FileConfig) -> Self {
        let defaults = Self::default();
        Self {
            bridge_port: file
                .bridge
                .as_ref()
                .and_then(|section| section.port)
                .map(|value| value.to_string())
                .unwrap_or(defaults.bridge_port),
            alma_api: file
                .alma
                .as_ref()
                .and_then(|section| section.api.clone())
                .unwrap_or(defaults.alma_api),
            alma_model: file
                .alma
                .as_ref()
                .and_then(|section| section.model.clone())
                .unwrap_or_default(),
            alma_timeout: file
                .alma
                .as_ref()
                .and_then(|section| section.timeout)
                .map(|value| value.to_string())
                .unwrap_or(defaults.alma_timeout),
            alma_max_retries: file
                .alma
                .as_ref()
                .and_then(|section| section.max_retries)
                .map(|value| value.to_string())
                .unwrap_or(defaults.alma_max_retries),
            alma_retry_delay_ms: file
                .alma
                .as_ref()
                .and_then(|section| section.retry_delay_ms)
                .map(|value| value.to_string())
                .unwrap_or(defaults.alma_retry_delay_ms),
            onebot_api_timeout: file
                .onebot
                .as_ref()
                .and_then(|section| section.api_timeout)
                .map(|value| value.to_string())
                .unwrap_or(defaults.onebot_api_timeout),
            access_token: file
                .onebot
                .as_ref()
                .and_then(|section| section.access_token.clone())
                .unwrap_or_default(),
            group_history_size: file
                .chat
                .as_ref()
                .and_then(|section| section.group_history_size)
                .map(|value| value.to_string())
                .unwrap_or(defaults.group_history_size),
            thinking_message: file
                .chat
                .as_ref()
                .and_then(|section| section.thinking_message.clone())
                .unwrap_or_default(),
            show_thinking: file
                .chat
                .as_ref()
                .and_then(|section| section.show_thinking)
                .unwrap_or(defaults.show_thinking),
            show_tool_calls: file
                .chat
                .as_ref()
                .and_then(|section| section.show_tool_calls)
                .unwrap_or(defaults.show_tool_calls),
            segmented_replies: file
                .chat
                .as_ref()
                .and_then(|section| section.segmented_replies)
                .unwrap_or(defaults.segmented_replies),
            people_dir: file
                .people
                .as_ref()
                .and_then(|section| section.dir.clone())
                .unwrap_or(defaults.people_dir),
            db_path: file
                .database
                .as_ref()
                .and_then(|section| section.path.clone())
                .unwrap_or(defaults.db_path),
        }
    }

    fn to_file(&self) -> FileConfig {
        FileConfig {
            bridge: Some(BridgeSection {
                port: self.bridge_port.parse().ok(),
            }),
            alma: Some(AlmaSection {
                api: Some(self.alma_api.trim().to_string()),
                model: non_empty(self.alma_model.trim()),
                timeout: self.alma_timeout.parse().ok(),
                max_retries: self.alma_max_retries.parse().ok(),
                retry_delay_ms: self.alma_retry_delay_ms.parse().ok(),
            }),
            database: Some(DatabaseSection {
                path: non_empty(self.db_path.trim())
                    .or_else(|| Some("bridge-state.db".to_string())),
            }),
            people: Some(PeopleSection {
                dir: non_empty(self.people_dir.trim()),
            }),
            onebot: Some(OneBotSection {
                api_timeout: self.onebot_api_timeout.parse().ok(),
                access_token: non_empty(self.access_token.trim()),
            }),
            chat: Some(ChatSection {
                group_history_size: self.group_history_size.parse().ok(),
                thinking_message: non_empty(self.thinking_message.trim()),
                show_thinking: Some(self.show_thinking),
                show_tool_calls: Some(self.show_tool_calls),
                segmented_replies: Some(self.segmented_replies),
            }),
        }
    }
}

fn non_empty(value: &str) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

#[derive(Default, Deserialize, Serialize)]
struct FileConfig {
    bridge: Option<BridgeSection>,
    alma: Option<AlmaSection>,
    database: Option<DatabaseSection>,
    people: Option<PeopleSection>,
    onebot: Option<OneBotSection>,
    chat: Option<ChatSection>,
}

#[derive(Default, Deserialize, Serialize)]
struct BridgeSection {
    port: Option<u16>,
}

#[derive(Default, Deserialize, Serialize)]
struct AlmaSection {
    api: Option<String>,
    model: Option<String>,
    timeout: Option<u64>,
    max_retries: Option<u32>,
    retry_delay_ms: Option<u64>,
}

#[derive(Default, Deserialize, Serialize)]
struct DatabaseSection {
    path: Option<String>,
}

#[derive(Default, Deserialize, Serialize)]
struct PeopleSection {
    dir: Option<String>,
}

#[derive(Default, Deserialize, Serialize)]
struct OneBotSection {
    api_timeout: Option<u64>,
    access_token: Option<String>,
}

#[derive(Default, Deserialize, Serialize)]
struct ChatSection {
    group_history_size: Option<usize>,
    thinking_message: Option<String>,
    show_thinking: Option<bool>,
    show_tool_calls: Option<bool>,
    segmented_replies: Option<bool>,
}
