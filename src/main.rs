use std::env;

use alma_onebot_bridge::{
    BridgeRunOptions, Config, apply_debugger_defaults, init_tracing, run_bridge_until,
    shutdown_signal,
};

#[tokio::main]
async fn main() {
    let _tracing_guard = init_tracing();

    let debugger_mode = env::args().any(|arg| arg == "--debugger");
    let mut config = Config::load();
    if debugger_mode {
        apply_debugger_defaults(&mut config);
    }

    let options = BridgeRunOptions {
        debugger_mode: false,
        write_pid_file: true,
    };

    if let Err(e) = run_bridge_until(config, options, shutdown_signal()).await {
        tracing::error!("{e}");
        std::process::exit(1);
    }
}
