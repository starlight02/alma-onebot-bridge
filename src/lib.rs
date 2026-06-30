mod alma;
mod alma_ws;
mod auth;
pub mod config;
mod face_map;
mod group_log;
mod handlers;
mod onebot;
mod people;
mod pipeline;
mod server;
mod state;

#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use std::env;
use std::ffi::OsStr;
use std::fs;
use std::future::Future;
use std::io;
use std::net::TcpListener;
use std::path::{Path, PathBuf};

use crate::alma_ws::AlmaWsClient;
pub use crate::config::Config;
use crate::state::SharedState;
#[cfg(unix)]
use smol::channel;
use trillium_smol::SmolRuntime;

const LOG_ROTATE_BYTES: u64 = 10 * 1024 * 1024;
const LOG_BACKUP_COUNT: u8 = 3;

#[derive(Clone, Copy, Debug)]
pub struct BridgeRunOptions {
    pub debugger_mode: bool,
    pub write_pid_file: bool,
}

impl Default for BridgeRunOptions {
    fn default() -> Self {
        Self {
            debugger_mode: false,
            write_pid_file: true,
        }
    }
}

#[derive(Default)]
pub struct BridgeRunHooks {
    pub on_listening: Option<Box<dyn FnOnce() + Send + 'static>>,
}

/// PID file location for process discovery by desktop GUI shells.
pub fn pid_file_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config/alma/bridge/bridge.pid")
}

pub fn write_pid_file() {
    let path = pid_file_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let pid = std::process::id();
    match std::fs::write(&path, pid.to_string()) {
        Ok(_) => tracing::info!("  PID file    : {} (pid={})", path.display(), pid),
        Err(e) => tracing::warn!("Failed to write PID file {}: {}", path.display(), e),
    }
}

pub fn remove_pid_file() {
    let path = pid_file_path();
    match std::fs::remove_file(&path) {
        Ok(_) => tracing::info!("Removed PID file: {}", path.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!("Failed to remove PID file {}: {}", path.display(), e),
    }
}

pub fn init_tracing() -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let log_path = env::var_os("BRIDGE_LOG_FILE")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty());
    init_tracing_with_log_file(log_path)
}

pub fn init_tracing_with_log_file(
    log_path: Option<PathBuf>,
) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    let Some(log_path) = log_path else {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(env_filter)
            .try_init();
        return None;
    };

    let parent = log_path.parent().unwrap_or_else(|| Path::new("."));
    if let Err(e) = std::fs::create_dir_all(parent) {
        eprintln!(
            "Failed to create log directory {}; falling back to stderr: {}",
            parent.display(),
            e
        );
        let _ = tracing_subscriber::fmt()
            .with_env_filter(env_filter)
            .try_init();
        return None;
    }

    if let Err(e) = rotate_log_file_if_needed(&log_path, LOG_ROTATE_BYTES, LOG_BACKUP_COUNT) {
        eprintln!("Failed to rotate log file {}: {}", log_path.display(), e);
    }

    let file_name = log_path
        .file_name()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("bridge.log"));
    let file_appender = tracing_appender::rolling::never(parent, file_name);
    let (writer, guard) = tracing_appender::non_blocking(file_appender);
    let _ = tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_writer(writer)
        .try_init();
    Some(guard)
}

pub async fn run_bridge_until<F>(
    config: Config,
    options: BridgeRunOptions,
    shutdown_signal: F,
) -> Result<(), String>
where
    F: Future<Output = ()> + Send + 'static,
{
    run_bridge_until_with_hooks(config, options, shutdown_signal, BridgeRunHooks::default()).await
}

pub async fn run_bridge_until_with_hooks<F>(
    mut config: Config,
    options: BridgeRunOptions,
    shutdown_signal: F,
    mut hooks: BridgeRunHooks,
) -> Result<(), String>
where
    F: Future<Output = ()> + Send + 'static,
{
    if options.debugger_mode {
        apply_debugger_defaults(&mut config);
    }

    let preflight_addr = format!("0.0.0.0:{}", config.bridge_port);
    match TcpListener::bind(&preflight_addr) {
        Ok(listener) => drop(listener),
        Err(e) => {
            return Err(format!(
                "Cannot listen on {preflight_addr}: {e}. Stop the existing bridge or change bridge.port in config.toml."
            ));
        }
    }

    let state = SharedState::new(config.clone()).await.map_err(|e| {
        format!(
            "Failed to initialize state database: {e}. If another bridge/debugger is running, change database.path in config.toml or stop the existing process."
        )
    })?;

    tracing::info!("Alma OneBot Bridge starting...");
    if options.debugger_mode {
        tracing::info!("  Debugger    : enabled");
    }
    tracing::info!("  Bridge port : {}", config.bridge_port);
    tracing::info!("  Alma API    : {}", config.alma_api);
    tracing::info!("  People dir  : {:?}", config.people_dir);
    tracing::info!("  Database    : {:?}", config.db_path);
    if let Some(ref model) = config.alma_model {
        tracing::info!("  Model       : {} (config override)", model);
    }
    tracing::info!("  Group hist  : {} messages", config.group_history_size);
    if let Some(ref msg) = config.thinking_message {
        tracing::info!("  Thinking msg: \"{}\"", msg);
    }
    if config.show_thinking {
        tracing::info!("  Show thinking: enabled (thinking blocks sent as separate messages)");
    }
    tracing::info!(
        "  Show tool calls: {}",
        if config.show_tool_calls {
            "enabled"
        } else {
            "disabled"
        }
    );
    tracing::info!(
        "  Segmented replies: {}",
        if config.segmented_replies {
            "enabled"
        } else {
            "disabled"
        }
    );
    tracing::info!(
        "  Listen group messages: {}",
        if config.listen_group_messages {
            "enabled"
        } else {
            "disabled"
        }
    );
    tracing::info!(
        "  Respond to group messages: {}",
        if config.respond_to_group_messages {
            "enabled"
        } else {
            "disabled"
        }
    );

    if options.write_pid_file {
        write_pid_file();
        let original_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            remove_pid_file();
            original_hook(info);
        }));
    }

    match AlmaWsClient::connect(&config.alma_api).await {
        Ok(client) => {
            state.set_alma_ws(client).await;
            tracing::info!("  Alma WS     : connected");
        }
        Err(e) => {
            tracing::warn!("  Alma WS     : FAILED ({})", e);
            tracing::warn!("  Bridge will start but AI replies won't work until Alma WS connects.");
            tracing::warn!("  Make sure Alma is running (alma status).");
        }
    }

    match alma::fetch_default_model(&state).await {
        Ok(model) => {
            state.set_default_model(model).await;
        }
        Err(e) => {
            tracing::warn!("Failed to fetch default model: {} — using fallback", e);
        }
    }

    {
        let state = state.clone();
        SmolRuntime::default().spawn(async move {
            loop {
                SmolRuntime::default()
                    .delay(std::time::Duration::from_millis(500))
                    .await;
                let client = match state.get_alma_ws().await {
                    Some(c) => c,
                    None => continue,
                };
                while let Some(event) = client.try_recv_event().await {
                    let _ = state.alma_event_tx.broadcast(event).await;
                }
            }
        });
    }

    #[cfg(unix)]
    {
        let state = state.clone();
        let (sighup_tx, sighup_rx) = channel::unbounded::<()>();
        std::thread::spawn(move || {
            let mut signals =
                match signal_hook::iterator::Signals::new([signal_hook::consts::SIGHUP]) {
                    Ok(signals) => signals,
                    Err(e) => {
                        tracing::warn!("failed to install SIGHUP handler: {}", e);
                        return;
                    }
                };
            for _ in signals.forever() {
                let _ = sighup_tx.send_blocking(());
            }
        });
        SmolRuntime::default().spawn(async move {
            while sighup_rx.recv().await.is_ok() {
                tracing::info!("[SIGHUP] Reloading config from disk...");
                let new_config = Config::load();
                let mut cfg = state.config.write().await;
                let alma_api_changed = cfg.alma_api != new_config.alma_api;
                cfg.group_history_size = new_config.group_history_size;
                cfg.thinking_message = new_config.thinking_message;
                cfg.show_thinking = new_config.show_thinking;
                cfg.show_tool_calls = new_config.show_tool_calls;
                cfg.segmented_replies = new_config.segmented_replies;
                cfg.listen_group_messages = new_config.listen_group_messages;
                cfg.respond_to_group_messages = new_config.respond_to_group_messages;
                cfg.alma_run_timeout_secs = new_config.alma_run_timeout_secs;
                cfg.alma_max_retries = new_config.alma_max_retries;
                cfg.alma_retry_delay_ms = new_config.alma_retry_delay_ms;
                cfg.access_token = new_config.access_token;
                cfg.onebot_api_timeout_secs = new_config.onebot_api_timeout_secs;
                cfg.alma_model = new_config.alma_model;
                cfg.alma_api = new_config.alma_api;
                cfg.people_dir = new_config.people_dir;
                drop(cfg);
                if alma_api_changed {
                    state.clear_alma_ws().await;
                    tracing::info!(
                        "[SIGHUP] Alma API changed; dropped existing WebSocket client so the next request reconnects"
                    );
                }
                tracing::info!("[SIGHUP] Config hot-reload complete");
            }
        });
    }

    tracing::info!(
        "Listening on {} — waiting for OneBot reverse WS connection...",
        preflight_addr
    );
    if let Some(on_listening) = hooks.on_listening.take() {
        on_listening();
    }

    server::run_server_until(config.bridge_port, state, shutdown_signal).await?;

    if options.write_pid_file {
        remove_pid_file();
    }
    Ok(())
}

pub fn apply_debugger_defaults(config: &mut Config) {
    config.db_path = env::temp_dir().join(format!(
        "alma-onebot-bridge-debugger-{}.db",
        std::process::id()
    ));

    if let Some(port) = first_available_port(18090, 20) {
        config.bridge_port = port;
    }
}

fn first_available_port(start: u16, attempts: u16) -> Option<u16> {
    for offset in 0..attempts {
        let Some(port) = start.checked_add(offset) else {
            break;
        };
        if TcpListener::bind(("0.0.0.0", port)).is_ok() {
            return Some(port);
        }
    }
    None
}

pub async fn shutdown_signal() {
    let term = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut registered_any = false;

    for signal in signal_hook::consts::TERM_SIGNALS {
        match signal_hook::flag::register(*signal, std::sync::Arc::clone(&term)) {
            Ok(_) => {
                registered_any = true;
            }
            Err(e) => {
                tracing::warn!("failed to install shutdown handler for signal {signal}: {e}");
            }
        }
    }

    if !registered_any {
        tracing::warn!(
            "No shutdown signal handlers were installed; waiting for external process termination"
        );
        std::future::pending::<()>().await;
    }

    while !term.load(std::sync::atomic::Ordering::Relaxed) {
        SmolRuntime::default()
            .delay(std::time::Duration::from_millis(200))
            .await;
    }

    tracing::info!("Shutdown signal received; stopping HTTP/WebSocket server");
}

fn rotate_log_file_if_needed(path: &Path, max_bytes: u64, backups: u8) -> io::Result<bool> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };

    if metadata.len() <= max_bytes {
        return Ok(false);
    }

    if backups == 0 {
        fs::remove_file(path)?;
        return Ok(true);
    }

    let oldest = rotated_log_path(path, backups);
    if oldest.exists() {
        fs::remove_file(&oldest)?;
    }

    for index in (1..backups).rev() {
        let from = rotated_log_path(path, index);
        if from.exists() {
            fs::rename(&from, rotated_log_path(path, index + 1))?;
        }
    }

    fs::rename(path, rotated_log_path(path, 1))?;
    Ok(true)
}

fn rotated_log_path(path: &Path, index: u8) -> PathBuf {
    let file_name = path.file_name().unwrap_or_else(|| OsStr::new("bridge.log"));
    path.with_file_name(format!("{}.{}", file_name.to_string_lossy(), index))
}

#[cfg(test)]
mod tests {
    use super::{rotate_log_file_if_needed, rotated_log_path};
    use std::{env, fs};

    #[test]
    fn rotates_log_when_it_exceeds_limit() {
        let dir = env::temp_dir().join(format!(
            "alma-onebot-bridge-log-rotate-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let log = dir.join("bridge.log");

        fs::write(&log, b"current").unwrap();
        fs::write(rotated_log_path(&log, 1), b"previous").unwrap();

        let rotated = rotate_log_file_if_needed(&log, 3, 3).unwrap();

        assert!(rotated);
        assert!(!log.exists());
        assert_eq!(fs::read(rotated_log_path(&log, 1)).unwrap(), b"current");
        assert_eq!(fs::read(rotated_log_path(&log, 2)).unwrap(), b"previous");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn keeps_small_log_in_place() {
        let dir = env::temp_dir().join(format!(
            "alma-onebot-bridge-log-keep-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let log = dir.join("bridge.log");
        fs::write(&log, b"small").unwrap();

        let rotated = rotate_log_file_if_needed(&log, 10, 3).unwrap();

        assert!(!rotated);
        assert_eq!(fs::read(&log).unwrap(), b"small");

        let _ = fs::remove_dir_all(&dir);
    }
}
