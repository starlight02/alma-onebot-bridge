use std::env;

use alma_onebot_bridge::{
    BridgeRunOptions, Config, init_tracing, run_bridge_until, shutdown_signal,
};

fn main() {
    smol::block_on(async_main());
}

async fn async_main() {
    let _tracing_guard = init_tracing();

    let debugger_mode = env::args().any(|arg| arg == "--debugger");
    let config = Config::load();

    let options = BridgeRunOptions {
        debugger_mode,
        write_pid_file: !debugger_mode,
    };

    if let Err(e) = run_bridge_until(config, options, shutdown_signal()).await {
        tracing::error!("{e}");
        std::process::exit(1);
    }
}
