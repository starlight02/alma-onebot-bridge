mod alma;
mod alma_ws;
mod config;
mod face_map;
mod handlers;
mod onebot;
mod people;
mod pipeline;
mod state;

use warp::Filter;

use crate::alma_ws::AlmaWsClient;
use crate::config::Config;
use crate::state::SharedState;

#[tokio::main]
async fn main() {
    // Initialize tracing (respects RUST_LOG env var, defaults to info)
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let config = Config::from_env();
    let state = SharedState::new(config.clone())
        .await
        .expect("Failed to initialize state database");

    tracing::info!("Alma OneBot Bridge starting...");
    tracing::info!("  Bridge port : {}", config.bridge_port);
    tracing::info!("  Alma API    : {}", config.alma_api);
    tracing::info!("  People dir  : {:?}", config.people_dir);
    tracing::info!("  Database    : {:?}", config.db_path);
    if let Some(ref model) = config.alma_model {
        tracing::info!("  Model       : {} (ALMA_MODEL override)", model);
    }
    tracing::info!("  Group hist  : {} messages", config.group_history_size);
    if let Some(ref msg) = config.thinking_message {
        tracing::info!("  Thinking msg: \"{}\"", msg);
    }
    if config.show_thinking {
        tracing::info!("  Show thinking: enabled (thinking blocks sent as separate messages)");
    }

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

    // ── Routes ───────────────────────────────────────────────────────────

    // GET /health — simple health check
    let health = warp::path("health")
        .and(warp::get())
        .and_then(handlers::http::health_handler);

    // WS endpoint — accepts connections at /, /ws, and /onebot/v11/ws
    // Different OneBot implementations use different default paths
    // Extracts optional Authorization header for token validation
    let expected_token = state.config.access_token.clone();
    let ws_handler = {
        let state = state.clone();
        let expected_token = expected_token.clone();
        move |auth: Option<String>, ws: warp::ws::Ws| {
            let state = state.clone();
            let expected = expected_token.clone();
            ws.on_upgrade(move |socket| {
                handlers::ws::handle_ws_connection(socket, state, auth, expected)
            })
        }
    };

    // Match root path: ws://host:port/
    let ws_root = warp::path::end()
        .and(warp::header::optional::<String>("authorization"))
        .and(warp::ws())
        .map(ws_handler.clone());

    // Match /ws: ws://host:port/ws (NapCat/snowluma default)
    let ws_path = warp::path("ws")
        .and(warp::path::end())
        .and(warp::header::optional::<String>("authorization"))
        .and(warp::ws())
        .map(ws_handler.clone());

    // Match /onebot/v11/ws: ws://host:port/onebot/v11/ws (Lagrange default)
    let ws_onebot = warp::path("onebot")
        .and(warp::path("v11"))
        .and(warp::path("ws"))
        .and(warp::path::end())
        .and(warp::header::optional::<String>("authorization"))
        .and(warp::ws())
        .map(ws_handler);

    let routes = health.or(ws_root).or(ws_path).or(ws_onebot);

    tracing::info!(
        "Listening on 0.0.0.0:{} — waiting for OneBot reverse WS connection...",
        config.bridge_port
    );

    warp::serve(routes)
        .run(([0, 0, 0, 0], config.bridge_port))
        .await;
}
