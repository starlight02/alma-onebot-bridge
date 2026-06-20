//! Persistent WebSocket client for Alma's internal chat pipeline.
//!
//! Connects to `ws://localhost:23001/ws/threads` and sends `generate_response`
//! requests. This is the same protocol used by the Alma GUI and `alma run` CLI,
//! ensuring messages are persisted and visible in the sidebar, and that SOUL,
//! Memory, People Profiles, and Skills are all loaded by the full pipeline.
//!
//! Also forwards `message_added` events for bidirectional communication
//! (Alma GUI → QQ).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio::time::timeout;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, tungstenite::Message};
use tracing::{debug, error, info, warn};

type WsSink = futures_util::stream::SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>;
type WsStream = futures_util::stream::SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>;

/// An event from Alma's WebSocket (e.g., a new message added to a thread).
#[derive(Clone, Debug)]
pub struct AlmaEvent {
    pub event_type: String,
    pub thread_id: String,
    pub message_role: String, // "user" or "assistant"
    pub message_text: String,
    pub thinking_text: Option<String>,
    #[allow(dead_code)]
    pub message_id: String,
}

/// Tracks a single in-flight generation request.
struct PendingGeneration {
    /// Accumulated assistant text (text_append with partType "text")
    text: String,
    /// User message ID captured from message_added event (for retry support)
    user_message_id: Option<String>,
    /// Channel to send the final result: (response_text, user_message_id, thinking_content)
    result_tx: oneshot::Sender<Result<(String, Option<String>, Option<String>), String>>,
}

/// Shared map of thread_id -> pending generation.
type PendingMap = Arc<Mutex<HashMap<String, PendingGeneration>>>;

/// Per-thread mutex to serialize generate() calls for the same thread.
type GenerationGuards = Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>;

/// Persistent WebSocket client for Alma's chat pipeline.
#[derive(Clone)]
pub struct AlmaWsClient {
    /// Send messages to the WebSocket writer task
    ws_tx: mpsc::UnboundedSender<Message>,
    /// Pending generations keyed by thread_id
    pending: PendingMap,
    /// Channel receiver for Alma events (message_added, etc.)
    event_rx: Arc<Mutex<mpsc::UnboundedReceiver<AlmaEvent>>>,
    /// Per-thread guards to serialize generate() calls
    guards: GenerationGuards,
}

impl AlmaWsClient {
    /// Connect to Alma's WebSocket endpoint and start the reader/writer tasks.
    pub async fn connect(alma_api: &str) -> Result<Self, String> {
        let ws_url = alma_api
            .replace("http://", "ws://")
            .replace("https://", "wss://");
        let url = format!("{}/ws/threads", ws_url);

        info!("Connecting to Alma WebSocket: {}", url);

        let (ws_stream, _) = tokio_tungstenite::connect_async(&url)
            .await
            .map_err(|e| format!("WebSocket connect failed: {}", e))?;

        info!("Connected to Alma WebSocket");

        let (sink, stream) = ws_stream.split();

        // Channel for outgoing messages (bridge -> Alma)
        let (ws_tx, ws_rx) = mpsc::unbounded_channel::<Message>();

        // Channel for Alma events (message_added, etc.)
        let (event_tx, event_rx) = mpsc::unbounded_channel::<AlmaEvent>();

        // Pending generations map
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));

        // Per-thread generation guards
        let guards: GenerationGuards = Arc::new(Mutex::new(HashMap::new()));

        // Writer task: forwards messages from channel to WebSocket
        tokio::spawn(writer_task(sink, ws_rx));

        // Reader task: dispatches incoming events
        let pending_clone = pending.clone();
        tokio::spawn(reader_task(stream, pending_clone, event_tx));

        Ok(AlmaWsClient {
            ws_tx,
            pending,
            event_rx: Arc::new(Mutex::new(event_rx)),
            guards,
        })
    }

    /// Send a user message through the full Alma chat pipeline and collect the response.
    ///
    /// Returns `(response_text, user_message_id)` where `user_message_id` is the
    /// Alma-side message ID captured from `message_added` — used for retries via
    /// `retryOfMessageId` to avoid creating duplicate user messages.
    ///
    /// `model` — if Some, explicitly force a model; if None, Alma uses the thread's current model
    /// `source` — platform identifier for Alma server ("telegram", "telegram-group", etc.)
    /// `ephemeral_context` — per-turn system prompt additions (SENDER PROFILE, etc.)
    pub async fn generate(
        &self,
        thread_id: &str,
        model: Option<&str>,
        message: &str,
        file_parts: Vec<serde_json::Value>,
        timeout_secs: u64,
        source: &str,
        ephemeral_context: &str,
    ) -> Result<(String, Option<String>, Option<String>), String> {
        // Acquire per-thread guard to serialize generations for the same thread
        let guard = {
            let mut guards = self.guards.lock().await;
            guards
                .entry(thread_id.to_string())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let _guard_lock = guard.lock().await;

        // Check WS connectivity before sending
        if self.ws_tx.is_closed() {
            return Err("Alma WebSocket connection is closed".to_string());
        }

        let (tx, rx) = oneshot::channel();

        // Register this generation request
        {
            let mut map = self.pending.lock().await;
            if let Some(_existing) = map.remove(thread_id) {
                warn!(
                    "[AlmaWS] Overwriting existing pending generation for thread {} — previous generation was not resolved",
                    thread_id
                );
            }
            map.insert(
                thread_id.to_string(),
                PendingGeneration {
                    text: String::new(),
                    user_message_id: None,
                    result_tx: tx,
                },
            );
            debug!(
                "[AlmaWS] Registered pending generation for thread {} (pending count: {})",
                thread_id,
                map.len()
            );
        }

        // Send the generate_response request
        let mut parts = vec![json!({"type": "text", "text": message})];
        for part in file_parts {
            parts.push(part);
        }
        let mut data = json!({
            "threadId": thread_id,
            "userMessage": {
                "role": "user",
                "parts": parts
            },
            "source": source
        });
        if let Some(model) = model {
            data["model"] = json!(model);
        }

        if !ephemeral_context.is_empty() {
            data["ephemeralContext"] = json!(ephemeral_context);
        }

        let request = json!({
            "type": "generate_response",
            "data": data
        });

        let model_label = model.unwrap_or("(thread-default)");

        info!(
            "[AlmaWS] Sending generate_response for thread {} (source={}, model={}, msg={} chars, ctx={} chars)",
            thread_id,
            source,
            model_label,
            message.len(),
            ephemeral_context.len()
        );

        if let Err(e) = self.ws_tx.send(Message::Text(request.to_string().into())) {
            self.pending.lock().await.remove(thread_id);
            return Err(format!("WebSocket send failed: {}", e));
        }

        debug!("[AlmaWS] generate_response sent, awaiting result...");

        // Wait for the response with timeout
        match timeout(Duration::from_secs(timeout_secs), rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => {
                self.pending.lock().await.remove(thread_id);
                Err("Generation channel closed unexpectedly".to_string())
            }
            Err(_) => {
                self.pending.lock().await.remove(thread_id);
                Err(format!("Generation timed out after {}s", timeout_secs))
            }
        }
    }

    /// Receive the next Alma event (non-blocking).
    /// Returns None if no event is available right now.
    pub async fn try_recv_event(&self) -> Option<AlmaEvent> {
        let mut rx = self.event_rx.lock().await;
        rx.try_recv().ok()
    }

    /// Check if the WebSocket connection is alive.
    #[allow(dead_code)]
    pub fn is_connected(&self) -> bool {
        !self.ws_tx.is_closed()
    }
}

/// Writer task: forward messages from the channel to the WebSocket sink.
async fn writer_task(mut sink: WsSink, mut rx: mpsc::UnboundedReceiver<Message>) {
    while let Some(msg) = rx.recv().await {
        if let Err(e) = sink.send(msg).await {
            error!("Alma WS write error: {}", e);
            break;
        }
    }
    debug!("Alma WS writer task finished");
}

/// Reader task: parse incoming WebSocket messages and dispatch to pending generations + events.
async fn reader_task(
    mut stream: WsStream,
    pending: PendingMap,
    event_tx: mpsc::UnboundedSender<AlmaEvent>,
) {
    let mut generating_threads: HashSet<String> = HashSet::new();
    let mut pending_assistant_updates: HashMap<String, AlmaEvent> = HashMap::new();

    while let Some(result) = stream.next().await {
        let msg = match result {
            Ok(msg) => msg,
            Err(e) => {
                error!("Alma WS read error: {}", e);
                break;
            }
        };

        match msg {
            Message::Text(text) => {
                let json: Value = match serde_json::from_str(&text) {
                    Ok(v) => v,
                    Err(e) => {
                        warn!("Failed to parse Alma WS message: {}", e);
                        continue;
                    }
                };
                dispatch_event(
                    &json,
                    &pending,
                    &event_tx,
                    &mut generating_threads,
                    &mut pending_assistant_updates,
                )
                .await;
            }
            Message::Close(_) => {
                info!("Alma WebSocket closed");
                break;
            }
            _ => {} // Ignore ping/pong/binary
        }
    }

    // Connection lost — fail all pending generations
    let mut map = pending.lock().await;
    for (thread_id, pg) in map.drain() {
        let _ = pg
            .result_tx
            .send(Err("Alma WebSocket disconnected".to_string()));
        warn!(
            "Pending generation for thread {} failed: WS disconnected",
            thread_id
        );
    }

    debug!("Alma WS reader task finished");
}

/// Dispatch a parsed WebSocket event to pending generations and/or event channel.
async fn dispatch_event(
    msg: &Value,
    pending: &PendingMap,
    event_tx: &mpsc::UnboundedSender<AlmaEvent>,
    generating_threads: &mut HashSet<String>,
    pending_assistant_updates: &mut HashMap<String, AlmaEvent>,
) {
    let msg_type = msg.get("type").and_then(|t| t.as_str()).unwrap_or("");
    let data = match msg.get("data") {
        Some(d) => d,
        None => {
            debug!("[AlmaWS] Event with no data: type={}", msg_type);
            return;
        }
    };

    // Catch-all trace: log every event for protocol discovery
    {
        let tid = data
            .get("threadId")
            .or_else(|| data.get("id"))
            .and_then(|t| t.as_str())
            .unwrap_or("-");
        debug!("[AlmaWS] ← event: type={}, thread={}", msg_type, tid);
    }

    match msg_type {
        "message_delta" => {
            let thread_id = match data.get("threadId").and_then(|t| t.as_str()) {
                Some(id) => id,
                None => return,
            };

            let mut map = pending.lock().await;
            let pg = match map.get_mut(thread_id) {
                Some(g) => g,
                None => {
                    debug!(
                        "[AlmaWS] message_delta for thread {} — no pending generation (pending keys: {:?})",
                        thread_id,
                        map.keys().collect::<Vec<_>>()
                    );
                    return;
                }
            };

            if let Some(deltas) = data.get("deltas").and_then(|d| d.as_array()) {
                for delta in deltas {
                    let delta_type = delta.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    let part_type = delta.get("partType").and_then(|t| t.as_str()).unwrap_or("");

                    if delta_type == "text_append" && part_type == "text" {
                        if let Some(text) = delta.get("text").and_then(|t| t.as_str()) {
                            pg.text.push_str(text);
                        }
                    }
                }
            }
        }

        "thread_generating" => {
            let thread_id = match data.get("id").and_then(|t| t.as_str()) {
                Some(id) => id,
                None => return,
            };

            let is_generating = data
                .get("isGenerating")
                .and_then(|g| g.as_bool())
                .unwrap_or(true);

            debug!(
                "[AlmaWS] thread_generating: thread={}, isGenerating={}",
                thread_id, is_generating
            );

            if is_generating {
                generating_threads.insert(thread_id.to_string());
            } else {
                generating_threads.remove(thread_id);
                let mut had_pending_generation = false;
                let mut map = pending.lock().await;
                if let Some(pg) = map.remove(thread_id) {
                    had_pending_generation = true;
                    let normalized = normalize_assistant_text(&pg.text);
                    let visible_text =
                        strip_tag_blocks(&normalized, "<system-reminder>", "</system-reminder>");
                    let (clean_text, thinking) = extract_think_blocks(&visible_text);
                    let trimmed = clean_text.trim().to_string();
                    let thinking = thinking
                        .map(|t| t.trim().to_string())
                        .filter(|t| !t.is_empty());
                    let user_msg_id = pg.user_message_id.clone();

                    if trimmed.is_empty() {
                        let _ = pg
                            .result_tx
                            .send(Err("Empty response from Alma".to_string()));
                    } else {
                        info!(
                            "[AlmaWS] Generation complete for thread {} ({} chars, user_msg_id={:?}, thinking={})",
                            thread_id,
                            trimmed.len(),
                            user_msg_id,
                            thinking
                                .as_ref()
                                .map(|t| format!("{} chars", t.len()))
                                .unwrap_or("none".into())
                        );
                        let _ = pg.result_tx.send(Ok((trimmed, user_msg_id, thinking)));
                    }
                } else {
                    debug!(
                        "[AlmaWS] thread_generating isGenerating=false for thread {} — no pending generation (pending keys: {:?})",
                        thread_id,
                        map.keys().collect::<Vec<_>>()
                    );
                }
                drop(map);

                if let Some(event) = pending_assistant_updates.remove(thread_id) {
                    if had_pending_generation {
                        debug!(
                            "[AlmaWS] Discarding buffered assistant update for bridge-owned generation in thread {}",
                            thread_id
                        );
                    } else {
                        debug!(
                            "[AlmaWS] Releasing buffered assistant update for thread {}",
                            thread_id
                        );
                        let _ = event_tx.send(event);
                    }
                }
            }
        }

        "generation_error" => {
            let thread_id = match data.get("threadId").and_then(|t| t.as_str()) {
                Some(id) => id,
                None => return,
            };

            let error_msg = data
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("Unknown generation error")
                .to_string();

            let mut map = pending.lock().await;
            if let Some(pg) = map.remove(thread_id) {
                error!(
                    "[AlmaWS] Generation error for thread {}: {}",
                    thread_id, error_msg
                );
                let _ = pg.result_tx.send(Err(error_msg));
            } else {
                warn!(
                    "[AlmaWS] Generation error for thread {} — no pending: {}",
                    thread_id, error_msg
                );
            }
            drop(map);

            generating_threads.remove(thread_id);
            pending_assistant_updates.remove(thread_id);
        }

        // ── Bidirectional: forward message_updated events ────────────────
        // NOTE: We use message_updated (NOT message_added) because message_added
        // fires with empty text for assistant messages. message_updated fires after
        // the text is fully populated, carrying the complete content.
        "message_updated" => {
            let thread_id = match data.get("threadId").and_then(|t| t.as_str()) {
                Some(id) => id.to_string(),
                None => return,
            };

            let msg_data = match data.get("message") {
                Some(m) => m,
                None => return,
            };

            let role = msg_data
                .get("role")
                .and_then(|r| r.as_str())
                .unwrap_or("unknown")
                .to_string();

            let message_id = data
                .get("id")
                .or_else(|| msg_data.get("id"))
                .and_then(|i| i.as_str())
                .unwrap_or("")
                .to_string();

            // Extract text from message parts (only type:"text", skip reasoning/step-start)
            let normalized_text = normalize_assistant_text(&extract_text_from_parts(msg_data));
            let visible_text =
                strip_tag_blocks(&normalized_text, "<system-reminder>", "</system-reminder>");
            let (clean_text, thinking_text) = extract_think_blocks(&visible_text);
            let text = clean_text.trim().to_string();
            let thinking_text = thinking_text
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty());

            debug!(
                "[AlmaWS] message_updated: thread={}, role={}, id={}, text_len={}, thinking={}",
                thread_id,
                role,
                message_id,
                text.len(),
                thinking_text
                    .as_ref()
                    .map(|t| format!("{} chars", t.len()))
                    .unwrap_or_else(|| "none".to_string())
            );

            if !text.is_empty() || thinking_text.is_some() {
                // Skip forwarding during active generation — the bridge pipeline
                // sends replies directly to QQ. We only forward the final
                // message_updated that fires AFTER generation completes
                // (e.g., messages typed in Alma GUI without bridge involvement).
                let event = AlmaEvent {
                    event_type: "message_updated".to_string(),
                    thread_id: thread_id.clone(),
                    message_role: role.clone(),
                    message_text: text,
                    thinking_text,
                    message_id,
                };

                if role == "assistant" && generating_threads.contains(&thread_id) {
                    debug!(
                        "[AlmaWS] message_updated: buffering assistant update while thread {} is generating",
                        thread_id
                    );
                    pending_assistant_updates.insert(thread_id, event);
                    return;
                }

                let _ = event_tx.send(event);
            }
        }

        // ── Capture the Alma-side user message ID for the current pending turn ──
        // message_added fires when Alma saves the user message to the thread.
        // We keep the ID on the pending generation mainly for observability.
        "message_added" => {
            let thread_id = match data.get("threadId").and_then(|t| t.as_str()) {
                Some(id) => id,
                None => return,
            };

            let role = data
                .get("message")
                .and_then(|m| m.get("role"))
                .and_then(|r| r.as_str())
                .unwrap_or("");

            if role == "user" {
                let msg_id = data
                    .get("id")
                    .or_else(|| data.get("message").and_then(|m| m.get("id")))
                    .and_then(|i| i.as_str());

                if let Some(id) = msg_id {
                    // Store in PendingGeneration for the success path
                    let mut map = pending.lock().await;
                    if let Some(pg) = map.get_mut(thread_id) {
                        pg.user_message_id = Some(id.to_string());
                    }
                    drop(map);

                    debug!(
                        "[AlmaWS] Captured user message ID: {} for thread {}",
                        id, thread_id
                    );
                }
            }
        }

        // Progress events — just log at debug level
        "tool_analysis_progress"
        | "memory_retrieval_progress"
        | "skill_analysis_progress"
        | "generation_completed" => {
            if let Some(thread_id) = data.get("threadId").and_then(|t| t.as_str()) {
                debug!("[AlmaWS] Progress ({}) for thread {}", msg_type, thread_id);
            }
        }

        // Thread state change events — log for debugging
        "thread_updated" | "thread_created" | "thread_deleted" => {
            let thread_id = data
                .get("id")
                .or_else(|| data.get("threadId"))
                .and_then(|t| t.as_str())
                .unwrap_or("?");
            debug!(
                "[AlmaWS] Thread event: type={}, thread={}",
                msg_type, thread_id
            );
        }

        // Log any unknown event types for protocol discovery
        other => {
            let thread_id = data
                .get("threadId")
                .or_else(|| data.get("id"))
                .and_then(|t| t.as_str())
                .unwrap_or("?");
            debug!(
                "[AlmaWS] Unknown event: type={}, thread={}",
                other, thread_id
            );
        }
    }
}

/// Extract text content from message parts array.
fn extract_text_from_parts(msg_data: &Value) -> String {
    let parts = match msg_data.get("parts").and_then(|p| p.as_array()) {
        Some(p) => p,
        None => return String::new(),
    };

    let mut text = String::new();
    for part in parts {
        let ptype = part.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if ptype == "text" {
            if let Some(t) = part.get("text").and_then(|t| t.as_str()) {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(t);
            }
        }
    }
    text
}

/// Strip `<think>...</think>` and `<thinking>...</thinking>` blocks from text.
/// Returns `(clean_text, thinking_content)`.
/// If multiple think blocks exist, their contents are joined with newlines.
pub(crate) fn extract_think_blocks(text: &str) -> (String, Option<String>) {
    const THINK_OPEN: &str = "<th\x69nk>";
    const THINK_CLOSE: &str = "</th\x69nk>";
    const THINKING_OPEN: &str = "<th\x69nking>";
    const THINKING_CLOSE: &str = "</th\x69nking>";

    let mut clean = String::with_capacity(text.len());
    let mut thinking_parts: Vec<String> = Vec::new();
    let mut in_think = false;
    let mut current_think = String::new();
    let mut active_end_tag: &str = "";

    let mut remaining = text;
    while !remaining.is_empty() {
        if in_think {
            if let Some(end_idx) = remaining.find(active_end_tag) {
                current_think.push_str(&remaining[..end_idx]);
                remaining = &remaining[end_idx + active_end_tag.len()..];
                in_think = false;
                let trimmed = current_think.trim().to_string();
                if !trimmed.is_empty() {
                    thinking_parts.push(trimmed);
                }
                current_think.clear();
                remaining = remaining.trim_start_matches('\n');
            } else {
                // Unclosed tag — treat rest as thinking
                current_think.push_str(remaining);
                let trimmed = current_think.trim().to_string();
                if !trimmed.is_empty() {
                    thinking_parts.push(trimmed);
                }
                break;
            }
        } else {
            let think_pos = remaining.find(THINK_OPEN);
            let thinking_pos = remaining.find(THINKING_OPEN);
            let next_start = match (think_pos, thinking_pos) {
                (Some(a), Some(b)) => {
                    if a <= b {
                        Some((THINK_CLOSE, THINK_OPEN, a))
                    } else {
                        Some((THINKING_CLOSE, THINKING_OPEN, b))
                    }
                }
                (Some(a), None) => Some((THINK_CLOSE, THINK_OPEN, a)),
                (None, Some(b)) => Some((THINKING_CLOSE, THINKING_OPEN, b)),
                (None, None) => None,
            };

            if let Some((end_tag, start_tag, start_idx)) = next_start {
                clean.push_str(&remaining[..start_idx]);
                remaining = &remaining[start_idx + start_tag.len()..];
                in_think = true;
                active_end_tag = end_tag;
            } else {
                clean.push_str(remaining);
                break;
            }
        }
    }

    let thinking = if thinking_parts.is_empty() {
        None
    } else {
        Some(thinking_parts.join("\n\n"))
    };

    (clean, thinking)
}

pub(crate) fn normalize_assistant_text(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace("<br />", "\n")
        .replace("<br/>", "\n")
        .replace("<br>", "\n")
}

fn strip_tag_blocks(text: &str, start_tag: &str, end_tag: &str) -> String {
    let mut out = String::new();
    let mut remaining = text;

    loop {
        if let Some(start) = remaining.find(start_tag) {
            out.push_str(&remaining[..start]);
            let after_start = &remaining[start + start_tag.len()..];
            if let Some(end) = after_start.find(end_tag) {
                remaining = &after_start[end + end_tag.len()..];
            } else {
                break;
            }
        } else {
            out.push_str(remaining);
            break;
        }
    }

    out
}

pub(crate) fn sanitize_visible_assistant_text(text: &str) -> String {
    let normalized = normalize_assistant_text(text);
    let without_system_reminders =
        strip_tag_blocks(&normalized, "<system-reminder>", "</system-reminder>");
    let (clean, _) = extract_think_blocks(&without_system_reminders);
    clean.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::{extract_think_blocks, normalize_assistant_text, sanitize_visible_assistant_text};

    #[test]
    fn normalizes_html_breaks_and_separates_thinking() {
        let normalized =
            normalize_assistant_text("<think>step 1<br>step 2</think><br>hello<br/>world");
        let (clean, thinking) = extract_think_blocks(&normalized);

        assert_eq!(clean.trim(), "hello\nworld");
        assert_eq!(thinking.as_deref(), Some("step 1\nstep 2"));
    }

    #[test]
    fn strips_unclosed_think_block_from_visible_text() {
        let normalized = normalize_assistant_text("visible<think>hidden");
        let (clean, thinking) = extract_think_blocks(&normalized);

        assert_eq!(clean, "visible");
        assert_eq!(thinking.as_deref(), Some("hidden"));
    }

    #[test]
    fn strips_system_reminder_block_from_visible_text() {
        let text = "正常内容\n<system-reminder>\n内部提醒\n</system-reminder>\n更多内容";
        let clean = sanitize_visible_assistant_text(text);

        assert_eq!(clean, "正常内容\n\n更多内容");
    }

    #[test]
    fn strips_system_reminder_and_think_together() {
        let text = "<think>hidden</think>hello<system-reminder>internal</system-reminder>world";
        let clean = sanitize_visible_assistant_text(text);

        assert_eq!(clean, "helloworld");
    }
}
