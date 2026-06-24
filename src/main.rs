mod alma;
mod alma_ws;
mod auth;
mod config;
mod face_map;
mod group_log;
mod handlers;
mod onebot;
mod people;
mod pipeline;
mod state;

use std::collections::HashMap;
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::net::TcpListener;
use std::path::{Path, PathBuf};

use warp::Filter;

use crate::alma_ws::AlmaWsClient;
use crate::config::Config;
use crate::state::SharedState;

const LOG_ROTATE_BYTES: u64 = 10 * 1024 * 1024;
const LOG_BACKUP_COUNT: u8 = 3;

/// PID file location for process discovery by the macOS GUI shell.
fn pid_file_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config/alma/bridge/bridge.pid")
}

fn write_pid_file() {
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

fn remove_pid_file() {
    let path = pid_file_path();
    match std::fs::remove_file(&path) {
        Ok(_) => tracing::info!("Removed PID file: {}", path.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!("Failed to remove PID file {}: {}", path.display(), e),
    }
}

#[tokio::main]
async fn main() {
    let _tracing_guard = init_tracing();

    let debugger_mode = env::args().any(|arg| arg == "--debugger");
    let mut config = Config::load();
    if debugger_mode {
        apply_debugger_defaults(&mut config);
    }

    let bind_addr = ([0, 0, 0, 0], config.bridge_port);
    let preflight_addr = format!("0.0.0.0:{}", config.bridge_port);
    match TcpListener::bind(&preflight_addr) {
        Ok(listener) => drop(listener),
        Err(e) => {
            tracing::error!(
                "Cannot listen on {}: {}. Stop the existing bridge or change bridge.port in config.toml.",
                preflight_addr,
                e
            );
            std::process::exit(1);
        }
    }

    let state = match SharedState::new(config.clone()).await {
        Ok(state) => state,
        Err(e) => {
            tracing::error!("Failed to initialize state database: {}", e);
            tracing::error!(
                "If another bridge/debugger is running, change database.path in config.toml or stop the existing process."
            );
            std::process::exit(1);
        }
    };

    tracing::info!("Alma OneBot Bridge starting...");
    if debugger_mode {
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

    write_pid_file();

    // ── Ensure the PID file is cleaned up on any exit path ────────────────
    // A plain `return` from `main` (or the normal end of `warp::serve`) calls
    // `remove_pid_file()` directly. But a panic bypasses that, leaving a stale
    // PID file that the GUI misreads as "bridge running". Installing a panic
    // hook guarantees cleanup in that case too. The hook runs on the panicking
    // thread, which is acceptable because PID-file removal is fallible and
    // non-critical (best-effort).
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        remove_pid_file();
        original_hook(info);
    }));

    // ── Initialize Alma WebSocket client ─────────────────────────────────
    // Connect to Alma's internal WS endpoint for the full chat pipeline.
    // This ensures messages are persisted in the thread and visible in the GUI.
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

    // ── Fetch default model from Alma settings ───────────────────────────
    match alma::fetch_default_model(&state).await {
        Ok(model) => {
            state.set_default_model(model).await;
        }
        Err(e) => {
            tracing::warn!("Failed to fetch default model: {} — using fallback", e);
        }
    }

    // ── Bidirectional: drain Alma WS events → broadcast channel ─────────
    // The AlmaWsClient reader pushes message_added events to its internal channel.
    // This task drains them and broadcasts to all active OneBot connections.
    {
        let state = state.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                let client = match state.get_alma_ws().await {
                    Some(c) => c,
                    None => continue,
                };
                while let Some(event) = client.try_recv_event().await {
                    let _ = state.alma_event_tx.send(event);
                }
            }
        });
    }

    // ── SIGHUP: hot-reload config ──────────────────────────────────────────
    // macOS GUI shell sends SIGHUP after writing config.toml.
    // Only reload fields that don't require re-binding the server port.
    #[cfg(unix)]
    {
        let state = state.clone();
        tokio::spawn(async move {
            use tokio::signal::unix::{SignalKind, signal};
            let mut sighup =
                signal(SignalKind::hangup()).expect("failed to install SIGHUP handler");
            loop {
                sighup.recv().await;
                tracing::info!("[SIGHUP] Reloading config from disk...");
                let new_config = Config::load();
                let mut cfg = state.config.write().await;
                let alma_api_changed = cfg.alma_api != new_config.alma_api;
                cfg.group_history_size = new_config.group_history_size;
                cfg.thinking_message = new_config.thinking_message;
                cfg.show_thinking = new_config.show_thinking;
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

    // ── Routes ───────────────────────────────────────────────────────────

    // GET /health — simple health check
    let health = warp::path("health")
        .and(warp::get())
        .and_then(handlers::http::health_handler);

    let state_filter = {
        let state = state.clone();
        warp::any().map(move || state.clone())
    };

    let qq_groups = warp::path!("qq" / "groups")
        .and(warp::get())
        .and(state_filter.clone())
        .and(warp::header::optional::<String>("authorization"))
        .and(warp::query::<HashMap<String, String>>())
        .and(warp::addr::remote())
        .and_then(handlers::http::list_groups_handler);

    let qq_group_send = warp::path!("qq" / "group" / i64 / "send")
        .and(warp::post())
        .and(warp::body::content_length_limit(64 * 1024))
        .and(warp::body::json())
        .and(state_filter.clone())
        .and(warp::header::optional::<String>("authorization"))
        .and(warp::query::<HashMap<String, String>>())
        .and(warp::addr::remote())
        .and_then(handlers::http::send_group_message_handler);

    let qq_private_send = warp::path!("qq" / "private" / i64 / "send")
        .and(warp::post())
        .and(warp::body::content_length_limit(64 * 1024))
        .and(warp::body::json())
        .and(state_filter)
        .and(warp::header::optional::<String>("authorization"))
        .and(warp::query::<HashMap<String, String>>())
        .and(warp::addr::remote())
        .and_then(handlers::http::send_private_message_handler);

    // WS endpoint — accepts connections at /, /ws, and /onebot/v11/ws
    // Different OneBot implementations use different default paths
    // Extracts optional Authorization header for token validation
    let ws_handler = {
        let state = state.clone();
        move |auth: Option<String>, query: HashMap<String, String>, ws: warp::ws::Ws| {
            let state = state.clone();
            ws.on_upgrade(move |socket| {
                handlers::ws::handle_ws_connection(socket, state, auth, query)
            })
        }
    };

    // Match root path: ws://host:port/
    let ws_root = warp::path::end()
        .and(warp::header::optional::<String>("authorization"))
        .and(warp::query::<HashMap<String, String>>())
        .and(warp::ws())
        .map(ws_handler.clone());

    // Match /ws: ws://host:port/ws (NapCat/snowluma default)
    let ws_path = warp::path("ws")
        .and(warp::path::end())
        .and(warp::header::optional::<String>("authorization"))
        .and(warp::query::<HashMap<String, String>>())
        .and(warp::ws())
        .map(ws_handler.clone());

    // Match /onebot/v11/ws: ws://host:port/onebot/v11/ws (Lagrange default)
    let ws_onebot = warp::path("onebot")
        .and(warp::path("v11"))
        .and(warp::path("ws"))
        .and(warp::path::end())
        .and(warp::header::optional::<String>("authorization"))
        .and(warp::query::<HashMap<String, String>>())
        .and(warp::ws())
        .map(ws_handler);

    let routes = health
        .or(qq_groups)
        .or(qq_group_send)
        .or(qq_private_send)
        .or(ws_root)
        .or(ws_path)
        .or(ws_onebot);

    tracing::info!(
        "Listening on {} — waiting for OneBot reverse WS connection...",
        preflight_addr
    );

    warp::serve(routes)
        .bind(bind_addr)
        .await
        .graceful(shutdown_signal())
        .run()
        .await;

    remove_pid_file();
}

fn init_tracing() -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    let log_path = env::var_os("BRIDGE_LOG_FILE")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty());

    let Some(log_path) = log_path else {
        tracing_subscriber::fmt().with_env_filter(env_filter).init();
        return None;
    };

    let parent = log_path.parent().unwrap_or_else(|| Path::new("."));
    if let Err(e) = std::fs::create_dir_all(parent) {
        eprintln!(
            "Failed to create log directory {}; falling back to stderr: {}",
            parent.display(),
            e
        );
        tracing_subscriber::fmt().with_env_filter(env_filter).init();
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
    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_writer(writer)
        .init();
    Some(guard)
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

fn apply_debugger_defaults(config: &mut Config) {
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

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        let mut terminate =
            signal(SignalKind::terminate()).expect("failed to install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }

    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }

    tracing::info!("Shutdown signal received; stopping HTTP/WebSocket server");
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
