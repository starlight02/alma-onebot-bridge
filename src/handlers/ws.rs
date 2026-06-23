use futures_util::{SinkExt, StreamExt};
use std::collections::HashMap;
use tokio::sync::{mpsc, watch};
use tokio::time::{Duration, timeout};
use tokio_stream::wrappers::UnboundedReceiverStream;
use tracing::{debug, error, info, warn};
use warp::ws::{Message, WebSocket};

use crate::auth::is_ws_authorized;
use crate::onebot::{OneBotApiHandle, PendingCalls, try_resolve_api_response};
use crate::pipeline::{handle_alma_event, process_message_event};
use crate::state::SharedState;

const MAX_INCOMING_TEXT_BYTES: usize = 1_000_000;

/// Handle a new reverse WebSocket connection from the OneBot client.
/// Validates the current access token if configured.
pub async fn handle_ws_connection(
    ws: WebSocket,
    state: SharedState,
    auth_header: Option<String>,
    query: HashMap<String, String>,
) {
    // ── Access token validation ──────────────────────────────────────────
    let expected_token = state.config.read().await.access_token.clone();
    if !is_ws_authorized(expected_token.as_deref(), auth_header.as_deref(), &query) {
        warn!("WebSocket connection rejected: invalid or missing access token");
        return;
    }

    let connection_id = state.register_onebot_connection();
    info!(
        "OneBot client connected via WebSocket (connection_id={})",
        connection_id
    );

    let (ws_sink, ws_stream) = ws.split();

    // Channel for pushing messages TO the WebSocket (any task can send)
    let (ws_tx, ws_rx) = mpsc::unbounded_channel::<Message>();
    let mut ws_rx = UnboundedReceiverStream::new(ws_rx);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    // Pending API call correlation map
    let pending_calls = PendingCalls::new();
    state
        .set_onebot_api_handle(OneBotApiHandle {
            ws_tx: ws_tx.clone(),
            pending: pending_calls.clone(),
            connection_id,
        })
        .await;

    // ── Bidirectional: subscribe to Alma events → forward to QQ ──────────
    let mut alma_event_rx = state.alma_event_tx.subscribe();
    let fwd_state = state.clone();
    let fwd_tx = ws_tx.clone();
    let fwd_pending = pending_calls.clone();
    let forwarding = tokio::spawn(async move {
        loop {
            if !fwd_state.is_current_onebot_connection(connection_id) {
                info!(
                    "[Alma→QQ] Connection {} superseded; stopping forwarding task",
                    connection_id
                );
                break;
            }

            match alma_event_rx.recv().await {
                Ok(event) => {
                    if let Err(e) =
                        handle_alma_event(&event, &fwd_state, &fwd_tx, &fwd_pending).await
                    {
                        warn!("[Alma→QQ] Forwarding error: {}", e);
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    warn!("[Alma→QQ] Event receiver lagged {} events", n);
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // ── Writer task: channel → WebSocket ─────────────────────────────────
    let writer = tokio::spawn(async move {
        let mut sink = ws_sink;
        while let Some(msg) = ws_rx.next().await {
            if let Err(e) = SinkExt::send(&mut sink, msg).await {
                debug!("WS writer ended: {}", e);
                break;
            }
        }
        debug!("WS writer task finished");
    });

    // ── Reader task: WebSocket → dispatch ────────────────────────────────
    let mut stream = ws_stream;
    let mut self_id: Option<i64> = None;

    while let Some(result) = stream.next().await {
        if !state.is_current_onebot_connection(connection_id) {
            info!(
                "OneBot connection {} superseded by a newer connection; closing reader",
                connection_id
            );
            break;
        }

        let msg = match result {
            Ok(msg) => msg,
            Err(e) => {
                error!("WS read error: {}", e);
                break;
            }
        };

        if msg.is_close() {
            info!("Received WS close frame");
            break;
        }

        if !msg.is_text() {
            continue;
        }

        let text = match msg.to_str() {
            Ok(t) => t,
            Err(_) => continue,
        };

        if text.len() > MAX_INCOMING_TEXT_BYTES {
            warn!(
                "WS text frame too large ({} bytes > {}), closing connection {}",
                text.len(),
                MAX_INCOMING_TEXT_BYTES,
                connection_id
            );
            break;
        }

        let json: serde_json::Value = match serde_json::from_str(text) {
            Ok(j) => j,
            Err(e) => {
                warn!("Failed to parse WS message: {}", e);
                continue;
            }
        };

        // ── API response (has echo field) ────────────────────────────
        if json.get("echo").is_some() && json.get("retcode").is_some() {
            try_resolve_api_response(&json, &pending_calls).await;
            continue;
        }

        // ── Event (has post_type field) ──────────────────────────────
        if let Some(post_type) = json.get("post_type").and_then(|p| p.as_str()) {
            // Capture self_id from first event
            if self_id.is_none()
                && let Some(sid) = json.get("self_id").and_then(|s| s.as_i64())
            {
                self_id = Some(sid);
                info!("OneBot bot QQ ID: {}", sid);
            }

            match post_type {
                "message" => {
                    let event: crate::onebot::event::OneBotEvent =
                        match serde_json::from_value(json) {
                            Ok(e) => e,
                            Err(e) => {
                                warn!("Failed to parse message event: {}", e);
                                continue;
                            }
                        };
                    // Spawn a task so the reader is never blocked
                    let st = state.clone();
                    let tx = ws_tx.clone();
                    let pc = pending_calls.clone();
                    let mut shutdown = shutdown_rx.clone();
                    tokio::spawn(async move {
                        tokio::select! {
                            result = process_message_event(&event, &st, &tx, &pc) => {
                                if let Err(e) = result {
                                    error!("Message processing error: {}", e);
                                }
                            }
                            _ = shutdown.changed() => {
                                debug!("Message processing cancelled because OneBot connection closed");
                            }
                        }
                    });
                }
                "meta_event" => {
                    if let Some(meta_type) = json.get("meta_event_type").and_then(|m| m.as_str()) {
                        match meta_type {
                            "heartbeat" => {
                                debug!("Heartbeat received");
                            }
                            "lifecycle" => {
                                let sub = json
                                    .get("sub_type")
                                    .and_then(|s| s.as_str())
                                    .unwrap_or("unknown");
                                info!("Lifecycle event: {}", sub);
                            }
                            _ => {}
                        }
                    }
                }
                "notice" => {
                    let notice_type = json
                        .get("notice_type")
                        .and_then(|n| n.as_str())
                        .unwrap_or("unknown");
                    match notice_type {
                        "group_recall" => {
                            log_group_recall(&json);
                        }
                        "friend_recall" => {
                            log_friend_recall(&json);
                        }
                        _ => {
                            debug!("Notice event: {}", notice_type);
                        }
                    }
                }
                "request" => {
                    debug!(
                        "Request event: {:?}",
                        json.get("request_type").and_then(|r| r.as_str())
                    );
                }
                _ => {
                    debug!("Unknown post_type: {}", post_type);
                }
            }
        }
    }

    info!(
        "OneBot client disconnected{}",
        self_id
            .map(|id| format!(" (QQ: {})", id))
            .unwrap_or_default()
    );
    state.clear_onebot_api_handle(connection_id).await;
    let _ = shutdown_tx.send(true);
    pending_calls.close_all().await;
    let _ = ws_tx.send(Message::close());
    drop(ws_tx);
    forwarding.abort();
    if timeout(Duration::from_secs(2), writer).await.is_err() {
        warn!(
            "WS writer did not finish after close frame for connection {}; aborting",
            connection_id
        );
    }
}

fn json_i64(json: &serde_json::Value, key: &str) -> Option<i64> {
    json.get(key).and_then(|v| v.as_i64())
}

fn log_group_recall(json: &serde_json::Value) {
    let Some(group_id) = json_i64(json, "group_id") else {
        warn!("[Recall] group_recall missing group_id");
        return;
    };
    let Some(user_id) = json_i64(json, "user_id") else {
        warn!(
            "[Recall] group_recall missing user_id in group {}",
            group_id
        );
        return;
    };
    let Some(operator_id) = json_i64(json, "operator_id") else {
        warn!(
            "[Recall] group_recall missing operator_id for user {} in group {}",
            user_id, group_id
        );
        return;
    };
    let Some(msg_id) = json_i64(json, "message_id") else {
        warn!(
            "[Recall] group_recall missing message_id for user {} in group {}",
            user_id, group_id
        );
        return;
    };

    if user_id == operator_id {
        info!(
            "[Recall] User {} recalled message {} in group {}",
            user_id, msg_id, group_id
        );
    } else {
        info!(
            "[Recall] Admin {} recalled message {} from user {} in group {}",
            operator_id, msg_id, user_id, group_id
        );
    }
}

fn log_friend_recall(json: &serde_json::Value) {
    let Some(user_id) = json_i64(json, "user_id") else {
        warn!("[Recall] friend_recall missing user_id");
        return;
    };
    let Some(msg_id) = json_i64(json, "message_id") else {
        warn!(
            "[Recall] friend_recall missing message_id for user {}",
            user_id
        );
        return;
    };
    info!(
        "[Recall] User {} recalled private message {}",
        user_id, msg_id
    );
}
