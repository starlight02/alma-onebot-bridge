use std::sync::OnceLock;

use windows_sys::Win32::Globalization::GetUserDefaultLocaleName;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Language {
    En,
    ZhHans,
}

static LANGUAGE: OnceLock<Language> = OnceLock::new();

#[derive(Clone, Copy, Debug)]
pub enum Text {
    About,
    AboutWindowTitle,
    AccessToken,
    AlmaApi,
    AppAlreadyRunning,
    Author,
    BridgeService,
    BridgeStatus,
    Config,
    ConfigDir,
    DatabasePath,
    Disabled,
    GenerationTimeout,
    GroupHistorySize,
    ListenPort,
    ListenToGroupMessages,
    License,
    Log,
    MaxRetries,
    ModelOverride,
    OneBotAndChat,
    OneBotTimeout,
    Off,
    On,
    OpenBridgeLog,
    OpenConfig,
    OpenConfigDirectory,
    OpenLog,
    PeopleDirectory,
    Project,
    Quit,
    Restart,
    RestartBridge,
    RespondToGroupMessages,
    RetryDelayMs,
    Runtime,
    SaveAndRestartBridge,
    SegmentReplies,
    Settings,
    SettingsWindowName,
    SettingsWindowTitle,
    ShowThinkingInQq,
    ShowToolCalls,
    Start,
    StartBridge,
    Status,
    Stop,
    StopBridge,
    Storage,
    ThinkingMessage,
    Version,
}

pub fn tr(text: Text) -> &'static str {
    match language() {
        Language::ZhHans => zh_hans(text),
        Language::En => en(text),
    }
}

pub fn app_description() -> &'static str {
    match language() {
        Language::ZhHans => "由 Rust 和 WinUI 3 驱动的 Windows 原生托盘应用。",
        Language::En => "Windows native tray app powered by Rust and WinUI 3.",
    }
}

pub fn empty_model_placeholder() -> &'static str {
    match language() {
        Language::ZhHans => "留空则使用 Alma 默认模型",
        Language::En => "Leave empty to use Alma default",
    }
}

pub fn empty_disabled_placeholder() -> &'static str {
    match language() {
        Language::ZhHans => "留空则禁用",
        Language::En => "Leave empty to disable",
    }
}

pub fn status_stopped() -> &'static str {
    match language() {
        Language::ZhHans => "已停止",
        Language::En => "Stopped",
    }
}

pub fn status_starting(port: u16) -> String {
    match language() {
        Language::ZhHans => format!("正在启动：端口 {port}"),
        Language::En => format!("Starting on port {port}"),
    }
}

pub fn status_running(port: u16) -> String {
    match language() {
        Language::ZhHans => format!("运行中：端口 {port}"),
        Language::En => format!("Running on port {port}"),
    }
}

pub fn status_stopping() -> &'static str {
    match language() {
        Language::ZhHans => "正在停止",
        Language::En => "Stopping",
    }
}

pub fn status_failed(reason: &str) -> String {
    match language() {
        Language::ZhHans => format!("失败：{reason}"),
        Language::En => format!("Failed: {reason}"),
    }
}

pub fn status_port_check(base: &str, healthy: bool) -> String {
    match (language(), healthy) {
        (Language::ZhHans, true) => format!("{base} - 端口检查通过"),
        (Language::ZhHans, false) => format!("{base} - 端口检查失败"),
        (Language::En, true) => format!("{base} - port check passed"),
        (Language::En, false) => format!("{base} - port check failed"),
    }
}

pub fn invalid_settings_message() -> &'static str {
    match language() {
        Language::ZhHans => "部分设置无效。",
        Language::En => "Some settings are invalid.",
    }
}

pub fn saved_restart_message() -> &'static str {
    match language() {
        Language::ZhHans => "已保存，桥接服务正在重启。",
        Language::En => "Saved. Bridge restart requested.",
    }
}

pub fn save_failed_message(error: &str) -> String {
    match language() {
        Language::ZhHans => format!("保存失败：{error}"),
        Language::En => format!("Save failed: {error}"),
    }
}

pub fn window_open_failed(window_name: &str, log_path: &str) -> String {
    match language() {
        Language::ZhHans => {
            format!("无法打开{window_name}窗口。\n\n请检查桥接日志：\n{log_path}")
        }
        Language::En => {
            format!("Could not open the {window_name} window.\n\nCheck the bridge log:\n{log_path}")
        }
    }
}

pub fn open_target_failed(target: &str, error: &str) -> String {
    match language() {
        Language::ZhHans => format!("无法打开：\n{target}\n\n{error}"),
        Language::En => format!("Could not open:\n{target}\n\n{error}"),
    }
}

pub fn shell_execute_error(code: isize) -> String {
    match (language(), code) {
        (Language::ZhHans, 0) => "操作系统内存或资源不足。".to_string(),
        (Language::ZhHans, 2) => "目标文件不存在。".to_string(),
        (Language::ZhHans, 3) => "目标路径不存在。".to_string(),
        (Language::ZhHans, 5) => "访问被拒绝。".to_string(),
        (Language::ZhHans, 8) => "内存不足，无法完成操作。".to_string(),
        (Language::ZhHans, 26) => "发生共享冲突。".to_string(),
        (Language::ZhHans, 27) => "文件关联不完整或无效。".to_string(),
        (Language::ZhHans, 28) => "DDE 事务超时。".to_string(),
        (Language::ZhHans, 29) => "DDE 事务失败。".to_string(),
        (Language::ZhHans, 30) => "DDE 事务正忙。".to_string(),
        (Language::ZhHans, 31) => "没有应用与目标关联。".to_string(),
        (Language::ZhHans, 32) => "未找到动态链接库。".to_string(),
        (Language::ZhHans, other) => format!("ShellExecuteW 返回错误码 {other}。"),
        (Language::En, 0) => "The operating system is out of memory or resources.".to_string(),
        (Language::En, 2) => "The target file was not found.".to_string(),
        (Language::En, 3) => "The target path was not found.".to_string(),
        (Language::En, 5) => "Access was denied.".to_string(),
        (Language::En, 8) => "There is not enough memory to complete the operation.".to_string(),
        (Language::En, 26) => "A sharing violation occurred.".to_string(),
        (Language::En, 27) => "The file association is incomplete or invalid.".to_string(),
        (Language::En, 28) => "The DDE transaction timed out.".to_string(),
        (Language::En, 29) => "The DDE transaction failed.".to_string(),
        (Language::En, 30) => "The DDE transaction is busy.".to_string(),
        (Language::En, 31) => "No application is associated with the target.".to_string(),
        (Language::En, 32) => "The dynamic-link library was not found.".to_string(),
        (Language::En, other) => format!("ShellExecuteW returned error code {other}."),
    }
}

fn en(text: Text) -> &'static str {
    match text {
        Text::About => "About",
        Text::AboutWindowTitle => "About Alma OneBot Bridge",
        Text::AccessToken => "Access token",
        Text::AlmaApi => "Alma API",
        Text::AppAlreadyRunning => "Alma OneBot Bridge is already running in the notification area.",
        Text::Author => "Author",
        Text::BridgeService => "Bridge service",
        Text::BridgeStatus => "Bridge status",
        Text::Config => "Config",
        Text::ConfigDir => "Config dir",
        Text::DatabasePath => "Database path",
        Text::Disabled => "Disabled",
        Text::GenerationTimeout => "Generation timeout",
        Text::GroupHistorySize => "Group history size",
        Text::ListenPort => "Listen port",
        Text::ListenToGroupMessages => "Listen to group messages",
        Text::License => "License",
        Text::Log => "Log",
        Text::MaxRetries => "Max retries",
        Text::ModelOverride => "Model override",
        Text::OneBotAndChat => "OneBot and chat",
        Text::OneBotTimeout => "OneBot timeout",
        Text::Off => "Off",
        Text::On => "On",
        Text::OpenBridgeLog => "Open Bridge Log",
        Text::OpenConfig => "Open config",
        Text::OpenConfigDirectory => "Open Config Directory",
        Text::OpenLog => "Open log",
        Text::PeopleDirectory => "People directory",
        Text::Project => "Project",
        Text::Quit => "Quit",
        Text::Restart => "Restart",
        Text::RestartBridge => "Restart Bridge",
        Text::RespondToGroupMessages => "Respond to group messages",
        Text::RetryDelayMs => "Retry delay ms",
        Text::Runtime => "Runtime",
        Text::SaveAndRestartBridge => "Save and restart bridge",
        Text::SegmentReplies => "Segment replies",
        Text::Settings => "Settings",
        Text::SettingsWindowName => "Settings",
        Text::SettingsWindowTitle => "Alma OneBot Bridge",
        Text::ShowThinkingInQq => "Show thinking in QQ",
        Text::ShowToolCalls => "Show tool calls",
        Text::Start => "Start",
        Text::StartBridge => "Start Bridge",
        Text::Status => "Status",
        Text::Stop => "Stop",
        Text::StopBridge => "Stop Bridge",
        Text::Storage => "Storage",
        Text::ThinkingMessage => "Thinking message",
        Text::Version => "Version",
    }
}

fn zh_hans(text: Text) -> &'static str {
    match text {
        Text::About => "关于",
        Text::AboutWindowTitle => "关于 Alma OneBot Bridge",
        Text::AccessToken => "访问令牌",
        Text::AlmaApi => "Alma API",
        Text::AppAlreadyRunning => "Alma OneBot Bridge 已在通知区域运行。",
        Text::Author => "作者",
        Text::BridgeService => "桥接服务",
        Text::BridgeStatus => "桥接状态",
        Text::Config => "配置",
        Text::ConfigDir => "配置目录",
        Text::DatabasePath => "数据库路径",
        Text::Disabled => "禁用",
        Text::GenerationTimeout => "生成超时",
        Text::GroupHistorySize => "群聊历史条数",
        Text::ListenPort => "监听端口",
        Text::ListenToGroupMessages => "监听群聊消息",
        Text::License => "许可证",
        Text::Log => "日志",
        Text::MaxRetries => "最大重试次数",
        Text::ModelOverride => "模型覆盖",
        Text::OneBotAndChat => "OneBot 与聊天",
        Text::OneBotTimeout => "OneBot 超时",
        Text::Off => "关闭",
        Text::On => "开启",
        Text::OpenBridgeLog => "打开桥接日志",
        Text::OpenConfig => "打开配置",
        Text::OpenConfigDirectory => "打开配置目录",
        Text::OpenLog => "打开日志",
        Text::PeopleDirectory => "People 目录",
        Text::Project => "项目",
        Text::Quit => "退出",
        Text::Restart => "重启",
        Text::RestartBridge => "重启桥接服务",
        Text::RespondToGroupMessages => "响应群聊消息",
        Text::RetryDelayMs => "重试延迟（毫秒）",
        Text::Runtime => "运行时",
        Text::SaveAndRestartBridge => "保存并重启桥接服务",
        Text::SegmentReplies => "分段式回复",
        Text::Settings => "设置",
        Text::SettingsWindowName => "设置",
        Text::SettingsWindowTitle => "Alma OneBot Bridge",
        Text::ShowThinkingInQq => "在 QQ 中显示思考",
        Text::ShowToolCalls => "显示工具调用",
        Text::Start => "启动",
        Text::StartBridge => "启动桥接服务",
        Text::Status => "状态",
        Text::Stop => "停止",
        Text::StopBridge => "停止桥接服务",
        Text::Storage => "存储",
        Text::ThinkingMessage => "思考提示消息",
        Text::Version => "版本",
    }
}

fn language() -> Language {
    *LANGUAGE.get_or_init(detect_language)
}

fn detect_language() -> Language {
    if let Some(locale) = user_default_locale() {
        if is_simplified_chinese_locale(&locale) {
            return Language::ZhHans;
        }
    }

    for key in ["ALMA_ONEBOT_BRIDGE_LANG", "LANGUAGE", "LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(value) = std::env::var(key) {
            if is_simplified_chinese_locale(&value) {
                return Language::ZhHans;
            }
        }
    }

    Language::En
}

fn user_default_locale() -> Option<String> {
    let mut buffer = [0_u16; 85];
    let len = unsafe { GetUserDefaultLocaleName(buffer.as_mut_ptr(), buffer.len() as i32) };
    if len <= 1 {
        return None;
    }
    Some(String::from_utf16_lossy(&buffer[..(len as usize - 1)]))
}

fn is_simplified_chinese_locale(locale: &str) -> bool {
    let normalized = locale.replace('_', "-").to_ascii_lowercase();
    normalized.starts_with("zh-hans")
        || normalized.starts_with("zh-cn")
        || normalized.starts_with("zh-sg")
        || normalized == "zh"
}
