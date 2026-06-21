use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio::time::timeout;
use tracing::{debug, warn};
use warp::ws::Message;

use super::event::{
    ApiRequest, ApiResponse, MessageSegment, extract_files, extract_images, extract_media_summary,
    extract_text, text_to_segments,
};

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
        Ok(Ok(resp)) => validate_api_response(action, resp),
        Ok(Err(_)) => Err("response channel closed".to_string()),
        Err(_) => Err(format!("API call timeout: {} ({}s)", action, timeout_secs)),
    }
}

fn validate_api_response(action: &str, resp: ApiResponse) -> Result<ApiResponse, String> {
    if resp.status == "ok" && resp.retcode == 0 {
        return Ok(resp);
    }

    let detail = resp
        .message
        .as_deref()
        .or(resp.wording.as_deref())
        .unwrap_or("no error message");

    Err(format!(
        "OneBot API {} failed: status={}, retcode={}, {}",
        action, resp.status, resp.retcode, detail
    ))
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

/// Content of a quoted/replied message.
pub struct QuotedMessage {
    pub sender_name: String,
    pub sender_id: Option<i64>,
    pub text: String,
    pub image_urls: Vec<String>,
}

/// Fetch a message by its ID using the OneBot get_msg API.
/// Returns a QuotedMessage with text content on success.
pub async fn get_msg(
    ws_tx: &mpsc::UnboundedSender<Message>,
    pending: &PendingCalls,
    message_id: &str,
    timeout_secs: u64,
) -> Result<QuotedMessage, String> {
    let resp = call_api(
        ws_tx,
        pending,
        "get_msg",
        json!({"message_id": message_id}),
        timeout_secs,
    )
    .await?;

    let data = resp.data.ok_or("get_msg: no data")?;
    Ok(parse_quoted_message(&data))
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

/// Fetch a group title by group_id using the OneBot group list API.
pub async fn get_group_name(
    ws_tx: &mpsc::UnboundedSender<Message>,
    pending: &PendingCalls,
    group_id: i64,
    timeout_secs: u64,
) -> Result<Option<String>, String> {
    let resp = call_api(ws_tx, pending, "get_group_list", json!({}), timeout_secs).await?;
    let data = resp.data.ok_or("get_group_list: no data")?;
    let groups = data
        .as_array()
        .ok_or("get_group_list: response is not an array")?;

    for group in groups {
        let gid = group.get("group_id").and_then(value_as_i64);
        if gid != Some(group_id) {
            continue;
        }

        let name = group
            .get("group_name")
            .and_then(|n| n.as_str())
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .map(|n| n.to_string());
        return Ok(name);
    }

    Ok(None)
}

fn parse_quoted_message(data: &serde_json::Value) -> QuotedMessage {
    let sender = data.get("sender");
    let sender_name = sender
        .and_then(|s| s.get("card"))
        .and_then(|n| n.as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            sender
                .and_then(|s| s.get("nickname"))
                .and_then(|n| n.as_str())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or("unknown")
        .to_string();
    let sender_id = sender.and_then(|s| s.get("user_id")).and_then(value_as_i64);

    let segments: Vec<MessageSegment> = data
        .get("message")
        .cloned()
        .and_then(|m| serde_json::from_value(m).ok())
        .unwrap_or_default();

    let mut text = extract_text(&segments);
    let image_urls = extract_images(&segments);
    let has_images = !image_urls.is_empty();
    let files = extract_files(&segments);
    let media_lines = extract_media_summary(&segments);

    let mut hints = Vec::new();
    if has_images {
        hints.push(if image_urls.len() == 1 {
            "[photo attached]".to_string()
        } else {
            format!("[{} photos attached]", image_urls.len())
        });
    }
    for (filename, _) in &files {
        hints.push(format!("[file: {}]", filename));
    }
    for line in media_lines {
        if line != "[Image]" && line != "[File]" {
            hints.push(line);
        }
    }

    if !hints.is_empty() {
        let media_summary = hints.join("\n");
        if text.is_empty() {
            text = media_summary;
        } else {
            text = format!("{}\n{}", text, media_summary);
        }
    }

    if text.is_empty() {
        let raw = data
            .get("raw_message")
            .and_then(|r| r.as_str())
            .unwrap_or("")
            .trim();
        if !raw.is_empty() && !looks_like_cq_code(raw) {
            text = raw.to_string();
        } else if has_images {
            text = "[photo attached]".to_string();
        }
    }

    QuotedMessage {
        sender_name,
        sender_id,
        text,
        image_urls,
    }
}

fn looks_like_cq_code(text: &str) -> bool {
    let trimmed = text.trim();
    trimmed.starts_with("[CQ:") || trimmed.contains("[CQ:")
}

fn value_as_i64(value: &serde_json::Value) -> Option<i64> {
    match value {
        serde_json::Value::Number(n) => n.as_i64(),
        serde_json::Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_quoted_message, validate_api_response};
    use crate::onebot::event::ApiResponse;
    use serde_json::json;

    #[test]
    fn quoted_image_message_becomes_media_summary_not_cq_code() {
        let quoted = parse_quoted_message(&json!({
            "sender": { "nickname": "Alice" },
            "message": [
                {
                    "type": "image",
                    "data": {
                        "file": "abc.jpg",
                        "url": "https://example.com/abc.jpg"
                    }
                }
            ],
            "raw_message": "[CQ:image,file=abc.jpg,url=https://example.com/abc.jpg]"
        }));

        assert_eq!(quoted.sender_name, "Alice");
        assert_eq!(quoted.sender_id, None);
        assert_eq!(quoted.text, "[photo attached]");
        assert_eq!(quoted.image_urls, vec!["https://example.com/abc.jpg"]);
        assert!(!quoted.text.contains("[CQ:"));
    }

    #[test]
    fn quoted_text_and_media_are_both_preserved() {
        let quoted = parse_quoted_message(&json!({
            "sender": { "nickname": "Bob" },
            "message": [
                { "type": "text", "data": { "text": "看看这个" } },
                { "type": "image", "data": { "url": "https://example.com/img.png" } }
            ]
        }));

        assert_eq!(quoted.sender_id, None);
        assert_eq!(quoted.text, "看看这个\n[photo attached]");
        assert_eq!(quoted.image_urls, vec!["https://example.com/img.png"]);
    }

    #[test]
    fn quoted_sender_prefers_group_card_and_keeps_sender_id() {
        let quoted = parse_quoted_message(&json!({
            "sender": {
                "user_id": 123456,
                "nickname": "Alice",
                "card": "一群名片"
            },
            "message": [
                { "type": "text", "data": { "text": "收到" } }
            ]
        }));

        assert_eq!(quoted.sender_name, "一群名片");
        assert_eq!(quoted.sender_id, Some(123456));
        assert_eq!(quoted.text, "收到");
    }

    #[test]
    fn api_response_nonzero_retcode_is_error() {
        let err = validate_api_response(
            "send_msg",
            ApiResponse {
                status: "failed".to_string(),
                retcode: 1400,
                data: None,
                echo: Some("echo".to_string()),
                message: Some("bad request".to_string()),
                wording: None,
            },
        )
        .unwrap_err();

        assert!(err.contains("send_msg"));
        assert!(err.contains("retcode=1400"));
        assert!(err.contains("bad request"));
    }
}
