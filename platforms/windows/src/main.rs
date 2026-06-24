#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod app_state;
#[cfg(windows)]
mod config_model;
#[cfg(windows)]
mod shell;
#[cfg(windows)]
mod single_instance;
#[cfg(windows)]
mod tray;
#[cfg(windows)]
mod ui;

#[cfg(windows)]
fn main() -> windows_reactor::Result<()> {
    velopack::VelopackApp::build().run();

    let _single_instance = single_instance::SingleInstance::acquire("AlmaOneBotBridge.Windows")?;
    let state =
        app_state::AppState::new().expect("failed to initialize AlmaOneBotBridge app state");

    windows_reactor::bootstrap()?;
    windows_reactor::App::new().run_custom(move |_app| {
        tray::install(state.clone())?;
        state.start_bridge();
        Ok(())
    })
}

#[cfg(not(windows))]
fn main() {
    eprintln!("AlmaOneBotBridge Windows App can only run on Windows.");
}
