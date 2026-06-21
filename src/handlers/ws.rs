use futures_util::{FutureExt, SinkExt, StreamExt};
use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tracing::{debug, error, info, warn};
use warp::ws::{Message, WebSocket};

use crate::onebot::{PendingCalls, try_resolve_api_response};
use crate::pipeline::{handle_alma_event, process_message_event};
use crate::state::SharedState;

const MAX_INCOMING_TEXT_BYTES: usize = 1_000_000;

/// Handle a new reverse WebSocket connection from the OneBot client.
/// Validates access token if configured (expected_token is Some).
pub async fn handle_ws_connection(
    ws: WebSocket,
    state: SharedState,
    auth_header: Option<String>,
    query: HashMap<String, String>,
    expected_token: Option<String>,
) {
    // ── Access token validation ──────────────────────────────────────────
    if !is_authorized(expected_token.as_deref(), auth_header.as_deref(), &query) {
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

    // Pending API call correlation map
    let pending_calls = PendingCalls::new();

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
                    // Wrap in catch_unwind so a single panic doesn't kill
                    // the entire Alma→QQ forwarding channel (e.g. from
                    // UTF-8 boundary issues in dedup logic on CJK text).
                    let result = AssertUnwindSafe(handle_alma_event(
                        &event,
                        &fwd_state,
                        &fwd_tx,
                        &fwd_pending,
                    ))
                    .catch_unwind()
                    .await;
                    match result {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => {
                            warn!("[Alma→QQ] Forwarding error: {}", e);
                        }
                        Err(panic_info) => {
                            let msg = panic_info
                                .downcast_ref::<String>()
                                .map(|s| s.as_str())
                                .or_else(|| panic_info.downcast_ref::<&str>().copied())
                                .unwrap_or("(non-string panic)");
                            error!(
                                "[Alma→QQ] handle_alma_event panicked (thread={}): {}. \
                                 Forwarding task survives — next event will retry.",
                                event.thread_id, msg
                            );
                        }
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
            if self_id.is_none() {
                if let Some(sid) = json.get("self_id").and_then(|s| s.as_i64()) {
                    self_id = Some(sid);
                    info!("OneBot bot QQ ID: {}", sid);
                }
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
                    tokio::spawn(async move {
                        let result = AssertUnwindSafe(process_message_event(&event, &st, &tx, &pc))
                            .catch_unwind()
                            .await;
                        match result {
                            Ok(Ok(())) => {}
                            Ok(Err(e)) => error!("Message processing error: {}", e),
                            Err(panic_info) => {
                                let msg = panic_info
                                    .downcast_ref::<String>()
                                    .map(|s| s.as_str())
                                    .or_else(|| panic_info.downcast_ref::<&str>().copied())
                                    .unwrap_or("(non-string panic)");
                                error!(
                                    "process_message_event panicked for message_id={:?}: {}",
                                    event.message_id, msg
                                );
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
                            let group_id =
                                json.get("group_id").and_then(|g| g.as_i64()).unwrap_or(0);
                            let user_id = json.get("user_id").and_then(|u| u.as_i64()).unwrap_or(0);
                            let operator_id = json
                                .get("operator_id")
                                .and_then(|o| o.as_i64())
                                .unwrap_or(0);
                            let msg_id =
                                json.get("message_id").and_then(|m| m.as_i64()).unwrap_or(0);
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
                        "friend_recall" => {
                            let user_id = json.get("user_id").and_then(|u| u.as_i64()).unwrap_or(0);
                            let msg_id =
                                json.get("message_id").and_then(|m| m.as_i64()).unwrap_or(0);
                            info!(
                                "[Recall] User {} recalled private message {}",
                                user_id, msg_id
                            );
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
    forwarding.abort();
    writer.abort();
}

fn is_authorized(
    expected_token: Option<&str>,
    auth_header: Option<&str>,
    query: &HashMap<String, String>,
) -> bool {
    let Some(expected) = expected_token.map(str::trim).filter(|t| !t.is_empty()) else {
        return true;
    };

    auth_header
        .and_then(extract_authorization_token)
        .or_else(|| {
            query
                .get("access_token")
                .or_else(|| query.get("token"))
                .map(String::as_str)
        })
        .map(|token| token.trim() == expected)
        .unwrap_or(false)
}

fn extract_authorization_token(header: &str) -> Option<&str> {
    let trimmed = header.trim();
    trimmed
        .strip_prefix("Bearer ")
        .or_else(|| trimmed.strip_prefix("bearer "))
        .or_else(|| trimmed.strip_prefix("Token "))
        .or_else(|| trimmed.strip_prefix("token "))
        .or_else(|| {
            if trimmed.contains(' ') {
                None
            } else {
                Some(trimmed)
            }
        })
}

#[cfg(test)]
mod tests {
    use super::is_authorized;
    use std::collections::HashMap;

    #[test]
    fn auth_allows_when_token_not_configured_or_blank() {
        assert!(is_authorized(None, None, &HashMap::new()));
        assert!(is_authorized(Some("   "), None, &HashMap::new()));
    }

    #[test]
    fn auth_accepts_bearer_and_query_tokens() {
        let mut query = HashMap::new();
        query.insert("access_token".to_string(), "secret".to_string());

        assert!(is_authorized(
            Some("secret"),
            Some("Bearer secret"),
            &HashMap::new()
        ));
        assert!(is_authorized(Some("secret"), None, &query));
    }

    #[test]
    fn auth_rejects_invalid_token() {
        let mut query = HashMap::new();
        query.insert("access_token".to_string(), "wrong".to_string());

        assert!(!is_authorized(Some("secret"), Some("Bearer wrong"), &query));
        assert!(!is_authorized(Some("secret"), None, &HashMap::new()));
    }
}
