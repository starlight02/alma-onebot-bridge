use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio::time::timeout;
use tracing::{debug, warn};
use warp::ws::Message;

use super::event::{ApiRequest, ApiResponse, MessageSegment, text_to_segments};

/// Thread-safe holder for pending OneBot API calls, shared across all handler tasks.
#[derive(Clone)]
pub struct PendingCalls(
    pub Arc<Mutex<std::collections::HashMap<String, oneshot::Sender<ApiResponse>>>>,
);

impl PendingCalls {
    pub fn new() -> Self {
        PendingCalls(Arc::new(Mutex::new(std::collections::HashMap::new())))
    }
}

/// Send an API action through the WebSocket and wait for the correlated response.
///
/// Uses the `echo` field to match request → response.
pub async fn call_api(
    ws_tx: &mpsc::UnboundedSender<Message>,
    pending: &PendingCalls,
    action: &str,
    params: serde_json::Value,
    timeout_secs: u64,
) -> Result<ApiResponse, String> {
    let echo = format!("bridge-{}-{}", uuid::Uuid::new_v4(), action);

    let (resp_tx, resp_rx) = oneshot::channel();

    // Register pending call
    pending.0.lock().await.insert(echo.clone(), resp_tx);

    // Send the request through WS
    let request = ApiRequest {
        action: action.to_string(),
        params,
        echo: echo.clone(),
    };

    let msg_text =
        serde_json::to_string(&request).map_err(|e| format!("serialize error: {}", e))?;

    ws_tx
        .send(Message::text(msg_text))
        .map_err(|e| format!("ws send error: {}", e))?;

    debug!("API call sent: {} (echo={})", action, echo);

    // Wait for response with timeout
    let result = timeout(Duration::from_secs(timeout_secs), resp_rx).await;

    // Clean up pending entry
    pending.0.lock().await.remove(&echo);

    match result {
        Ok(Ok(resp)) => Ok(resp),
        Ok(Err(_)) => Err("response channel closed".to_string()),
        Err(_) => Err(format!("API call timeout: {} ({}s)", action, timeout_secs)),
    }
}

/// Route an incoming WS message: either an API response (has echo) or return None (event).
pub async fn try_resolve_api_response(
    msg: &serde_json::Value,
    pending: &PendingCalls,
) -> Option<()> {
    if let Some(echo) = msg.get("echo").and_then(|e| e.as_str()) {
        if let Some(sender) = pending.0.lock().await.remove(echo) {
            let response: ApiResponse = match serde_json::from_value(msg.clone()) {
                Ok(r) => r,
                Err(e) => {
                    warn!("Failed to parse API response: {}", e);
                    return Some(());
                }
            };
            debug!(
                "API response resolved: echo={}, status={}",
                echo, response.status
            );
            let _ = sender.send(response);
            return Some(());
        } else {
            debug!("No pending call for echo: {}", echo);
        }
    }
    None
}

/// Convenience: send a text message via OneBot send_msg API.
/// Automatically converts `[emoji:NAME]` patterns to face segments.
pub async fn send_text_message(
    ws_tx: &mpsc::UnboundedSender<Message>,
    pending: &PendingCalls,
    message_type: &str,
    target_id: i64,
    text: &str,
    timeout_secs: u64,
) -> Result<ApiResponse, String> {
    let segments = text_to_segments(text);
    let message = serde_json::to_value(&segments).map_err(|e| format!("serialize error: {}", e))?;

    let params = match message_type {
        "group" => json!({
            "message_type": "group",
            "group_id": target_id,
            "message": message
        }),
        _ => json!({
            "message_type": "private",
            "user_id": target_id,
            "message": message
        }),
    };

    call_api(ws_tx, pending, "send_msg", params, timeout_secs).await
}

/// Convenience: send a text message as a reply via OneBot send_msg API.
/// Prepends a reply segment to the message array so QQ clients show the quoted message.
/// If `at_user_id` is Some and message_type is "group", also adds an @mention segment.
pub async fn send_reply_message(
    ws_tx: &mpsc::UnboundedSender<Message>,
    pending: &PendingCalls,
    message_type: &str,
    target_id: i64,
    text: &str,
    reply_to_id: &str,
    at_user_id: Option<&str>,
    timeout_secs: u64,
) -> Result<ApiResponse, String> {
    let mut segments = vec![MessageSegment::reply(reply_to_id)];

    // Add @mention for group messages
    if message_type == "group" {
        if let Some(uid) = at_user_id {
            segments.push(MessageSegment::at(uid));
        }
    }

    segments.extend(text_to_segments(text));

    let message = serde_json::to_value(&segments).map_err(|e| format!("serialize error: {}", e))?;

    let params = match message_type {
        "group" => json!({
            "message_type": "group",
            "group_id": target_id,
            "message": message
        }),
        _ => json!({
            "message_type": "private",
            "user_id": target_id,
            "message": message
        }),
    };

    call_api(ws_tx, pending, "send_msg", params, timeout_secs).await
}

/// Fetch a message by its ID using the OneBot get_msg API.
/// Returns (sender_nickname, message_text) on success.
pub async fn get_msg(
    ws_tx: &mpsc::UnboundedSender<Message>,
    pending: &PendingCalls,
    message_id: &str,
    timeout_secs: u64,
) -> Result<(String, String), String> {
    let resp = call_api(
        ws_tx,
        pending,
        "get_msg",
        json!({"message_id": message_id}),
        timeout_secs,
    )
    .await?;

    let data = resp.data.ok_or("get_msg: no data")?;

    let sender_name = data
        .get("sender")
        .and_then(|s| s.get("nickname"))
        .and_then(|n| n.as_str())
        .unwrap_or("unknown")
        .to_string();

    // Extract plain text from message segments
    let text = data
        .get("message")
        .and_then(|m| m.as_array())
        .map(|segs| {
            segs.iter()
                .filter(|s| s.get("type").and_then(|t| t.as_str()) == Some("text"))
                .filter_map(|s| {
                    s.get("data")
                        .and_then(|d| d.get("text"))
                        .and_then(|t| t.as_str())
                })
                .collect::<Vec<_>>()
                .join("")
                .trim()
                .to_string()
        })
        .unwrap_or_default();

    // Fall back to raw_message if segments had no text
    let text = if text.is_empty() {
        data.get("raw_message")
            .and_then(|r| r.as_str())
            .unwrap_or("")
            .to_string()
    } else {
        text
    };

    Ok((sender_name, text))
}

/// Fetch forwarded/merged message content using the OneBot get_forward_msg API.
/// Returns a list of (sender_nickname, message_text) for each node in the forward.
pub async fn get_forward_msg(
    ws_tx: &mpsc::UnboundedSender<Message>,
    pending: &PendingCalls,
    forward_id: &str,
    timeout_secs: u64,
) -> Result<Vec<(String, String)>, String> {
    let resp = call_api(
        ws_tx,
        pending,
        "get_forward_msg",
        json!({"id": forward_id}),
        timeout_secs,
    )
    .await?;

    let data = resp.data.ok_or("get_forward_msg: no data")?;

    let nodes = data
        .get("message")
        .and_then(|m| m.as_array())
        .ok_or("get_forward_msg: no message array")?;

    let mut results = Vec::new();
    for node in nodes {
        let node_data = match node.get("data") {
            Some(d) => d,
            None => continue,
        };

        let nickname = node_data
            .get("nickname")
            .or_else(|| node_data.get("name"))
            .and_then(|n| n.as_str())
            .unwrap_or("unknown")
            .to_string();

        // Extract text from content array (which is itself a message segment array)
        let text = node_data
            .get("content")
            .and_then(|c| c.as_array())
            .map(|segs| {
                segs.iter()
                    .filter(|s| s.get("type").and_then(|t| t.as_str()) == Some("text"))
                    .filter_map(|s| {
                        s.get("data")
                            .and_then(|d| d.get("text"))
                            .and_then(|t| t.as_str())
                    })
                    .collect::<Vec<_>>()
                    .join("")
                    .trim()
                    .to_string()
            })
            .unwrap_or_default();

        if !text.is_empty() {
            results.push((nickname, text));
        }
    }

    Ok(results)
}
