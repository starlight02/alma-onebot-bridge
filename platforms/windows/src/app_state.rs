use std::fs;
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alma_onebot_bridge::{
    BridgeRunHooks, BridgeRunOptions, Config, init_tracing_with_log_file,
    run_bridge_until_with_hooks,
};
use smol::channel;

use crate::config_model::ConfigModel;
use crate::i18n;

#[derive(Clone, Debug)]
pub enum BridgeStatus {
    Stopped,
    Starting,
    Running,
    Stopping,
    Failed(String),
}

impl BridgeStatus {
    pub fn text(&self, port: u16) -> String {
        match self {
            Self::Stopped => i18n::status_stopped().to_string(),
            Self::Starting => i18n::status_starting(port),
            Self::Running => i18n::status_running(port),
            Self::Stopping => i18n::status_stopping().to_string(),
            Self::Failed(reason) => i18n::status_failed(reason),
        }
    }
}

#[derive(Clone, Debug)]
pub struct AppSnapshot {
    pub status: BridgeStatus,
    pub port: u16,
    pub healthy: bool,
    pub config_dir: PathBuf,
    pub config_file: PathBuf,
    pub log_file: PathBuf,
}

#[derive(Debug)]
struct BridgeInner {
    status: BridgeStatus,
    shutdown_tx: Option<channel::Sender<()>>,
}

pub struct AppState {
    inner: Mutex<BridgeInner>,
    config_dir: PathBuf,
    config_file: PathBuf,
    log_file: PathBuf,
    _tracing_guard: Option<tracing_appender::non_blocking::WorkerGuard>,
}

impl AppState {
    pub fn new() -> Result<Arc<Self>, String> {
        let home =
            dirs::home_dir().ok_or_else(|| "Failed to resolve home directory".to_string())?;
        let config_dir = home.join(".config/alma/bridge");
        let config_file = config_dir.join("config.toml");
        let log_file = config_dir.join("bridge.log");
        fs::create_dir_all(&config_dir)
            .map_err(|e| format!("Failed to create config directory: {e}"))?;

        if !config_file.exists() {
            ConfigModel::default()
                .save_to(&config_file)
                .map_err(|e| format!("Failed to create default config: {e}"))?;
        }

        std::env::set_current_dir(&config_dir)
            .map_err(|e| format!("Failed to set bridge working directory: {e}"))?;

        let tracing_guard = init_tracing_with_log_file(Some(log_file.clone()));

        Ok(Arc::new(Self {
            inner: Mutex::new(BridgeInner {
                status: BridgeStatus::Stopped,
                shutdown_tx: None,
            }),
            config_dir,
            config_file,
            log_file,
            _tracing_guard: tracing_guard,
        }))
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_file.clone()
    }

    pub fn config_dir(&self) -> PathBuf {
        self.config_dir.clone()
    }

    pub fn log_file(&self) -> PathBuf {
        self.log_file.clone()
    }

    pub fn start_bridge(self: &Arc<Self>) {
        let mut inner = self.inner.lock().expect("bridge state poisoned");
        if inner.shutdown_tx.is_some() {
            return;
        }

        let (shutdown_tx, shutdown_rx) = channel::bounded::<()>(1);
        inner.shutdown_tx = Some(shutdown_tx);
        inner.status = BridgeStatus::Starting;
        drop(inner);

        let state = Arc::clone(self);
        std::thread::spawn(move || {
            let config = Config::load();
            let options = BridgeRunOptions {
                debugger_mode: false,
                write_pid_file: true,
            };
            let hook_state = Arc::clone(&state);
            let hooks = BridgeRunHooks {
                on_listening: Some(Box::new(move || {
                    let mut inner = hook_state.inner.lock().expect("bridge state poisoned");
                    if matches!(inner.status, BridgeStatus::Starting) {
                        inner.status = BridgeStatus::Running;
                    }
                })),
            };
            let result = smol::block_on(run_bridge_until_with_hooks(
                config,
                options,
                async move {
                    let _ = shutdown_rx.recv().await;
                },
                hooks,
            ));

            let mut inner = state.inner.lock().expect("bridge state poisoned");
            inner.shutdown_tx = None;
            inner.status = match result {
                Ok(()) => BridgeStatus::Stopped,
                Err(reason) => BridgeStatus::Failed(reason),
            };
        });
    }

    pub fn stop_bridge(self: &Arc<Self>) {
        let mut inner = self.inner.lock().expect("bridge state poisoned");
        if let Some(shutdown_tx) = inner.shutdown_tx.take() {
            inner.status = BridgeStatus::Stopping;
            let _ = shutdown_tx.try_send(());
        } else {
            inner.status = BridgeStatus::Stopped;
        }
    }

    pub fn restart_bridge(self: &Arc<Self>) {
        self.stop_bridge();
        let state = Arc::clone(self);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(800));
            state.start_bridge();
        });
    }

    pub fn quit(self: &Arc<Self>) -> ! {
        self.stop_bridge();
        std::thread::sleep(Duration::from_millis(400));
        std::process::exit(0);
    }

    pub fn snapshot(&self) -> AppSnapshot {
        let model = ConfigModel::load_from(&self.config_file).unwrap_or_default();
        let port = model.bridge_port.parse::<u16>().unwrap_or(8090);
        let status = self
            .inner
            .lock()
            .expect("bridge state poisoned")
            .status
            .clone();
        let healthy = matches!(status, BridgeStatus::Running) && is_port_open(port);
        AppSnapshot {
            status,
            port,
            healthy,
            config_dir: self.config_dir.clone(),
            config_file: self.config_file.clone(),
            log_file: self.log_file.clone(),
        }
    }
}

impl AppSnapshot {
    pub fn status_line(&self) -> String {
        match (&self.status, self.healthy) {
            (BridgeStatus::Running, true) => {
                i18n::status_port_check(&self.status.text(self.port), true)
            }
            (BridgeStatus::Running, false) => {
                i18n::status_port_check(&self.status.text(self.port), false)
            }
            _ => self.status.text(self.port),
        }
    }
}

fn is_port_open(port: u16) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&addr, Duration::from_millis(250)).is_ok()
}
