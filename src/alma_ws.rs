//! Persistent WebSocket client for Alma's internal chat pipeline.
//!
//! Connects to `ws://localhost:23001/ws/threads` and sends `generate_response`
//! requests. This is the same protocol used by the Alma GUI and `alma run` CLI,
//! ensuring messages are persisted and visible in the sidebar, and that SOUL,
//! Memory, People Profiles, and Skills are all loaded by the full pipeline.
//!
//! Also forwards `message_added` events for bidirectional communication
//! (Alma GUI → QQ).

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use futures_util::{FutureExt, select};
use serde_json::{Value, json};
use smol::channel;
use smol::lock::Mutex;
use tracing::{debug, error, info, warn};
use trillium_smol::SmolRuntime;
use trillium_websockets::async_tungstenite::{self, WebSocketStream, tungstenite::Message};

type GenerationResponse = (String, Option<String>, Option<String>);
type GenerationResult = Result<GenerationResponse, String>;
type AlmaWsStream = WebSocketStream<async_tungstenite::smol::ConnectStream>;

/// An event from Alma's WebSocket (e.g., a new message added to a thread).
#[derive(Clone, Debug)]
pub struct AlmaEvent {
    pub event_type: String,
    pub thread_id: String,
    pub message_role: String, // "user" or "assistant"
    pub message_text: String,
    pub thinking_text: Option<String>,
}

/// Tracks a single in-flight generation request.
struct PendingGeneration {
    /// Local bridge-owned generation id. Alma does not currently echo a request
    /// id in stream events, so this protects timeout cleanup from removing a
    /// newer retry's pending slot.
    generation_id: u64,
    /// Accumulated assistant text (text_append with partType "text")
    text: String,
    /// User message ID captured from message_added event (for retry support)
    user_message_id: Option<String>,
    /// `thread_generating=false` can arrive just before `generation_error`.
    /// Keep an empty pending turn briefly so the real error is not swallowed.
    empty_response_grace_started: bool,
    /// Channel to send the final result: (response_text, user_message_id, thinking_content)
    result_tx: channel::Sender<GenerationResult>,
}

/// Shared map of thread_id -> pending generation.
type PendingMap = Arc<Mutex<HashMap<String, PendingGeneration>>>;

/// Per-thread mutex to serialize generate() calls for the same thread.
type GenerationGuards = Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>;
type BridgeSuppressions = Arc<Mutex<HashMap<String, Instant>>>;
type BridgeOwnedAssistantMessages = Arc<Mutex<HashMap<String, Instant>>>;

const RECENT_BRIDGE_GENERATION_TTL: Duration = Duration::from_secs(30);
const RECENT_BRIDGE_GENERATION_LIMIT: usize = 8;
const STOP_GENERATION_DRAIN_TIMEOUT: Duration = Duration::from_secs(20);
const STOP_GENERATION_DRAIN_POLL: Duration = Duration::from_millis(100);
const STALE_BRIDGE_GENERATION_SUPPRESSION_TTL: Duration = Duration::from_secs(120);
const BRIDGE_OWNED_ASSISTANT_MESSAGE_TTL: Duration = Duration::from_secs(60 * 60);

#[derive(Clone, Debug)]
struct RecentBridgeGeneration {
    text: String,
    completed_at: Instant,
}

/// Persistent WebSocket client for Alma's chat pipeline.
#[derive(Clone)]
pub struct AlmaWsClient {
    /// Send messages to the WebSocket writer task
    ws_tx: channel::Sender<Message>,
    /// Pending generations keyed by thread_id
    pending: PendingMap,
    /// Channel receiver for Alma events (message_added, etc.)
    event_rx: Arc<Mutex<channel::Receiver<AlmaEvent>>>,
    /// Per-thread guards to serialize generate() calls
    guards: GenerationGuards,
    /// Monotonic local id for bridge-owned generation attempts.
    generation_seq: Arc<AtomicU64>,
    /// Threads with bridge-owned generation activity whose Alma updates should
    /// not be treated as GUI-authored outbound messages.
    bridge_suppressions: BridgeSuppressions,
    /// Current transport state. The client object survives reconnects.
    connected: Arc<AtomicBool>,
}

impl AlmaWsClient {
    /// Connect to Alma's WebSocket endpoint and start the reader/writer tasks.
    pub async fn connect(alma_api: &str) -> Result<Self, String> {
        let ws_url = alma_api
            .replace("http://", "ws://")
            .replace("https://", "wss://");
        let url = format!("{}/ws/threads", ws_url);

        info!("Connecting to Alma WebSocket: {}", url);

        let (ws_stream, _) = async_tungstenite::smol::connect_async(&url)
            .await
            .map_err(|e| format!("WebSocket connect failed: {}", e))?;

        info!("Connected to Alma WebSocket");

        // Channel for outgoing messages (bridge -> Alma)
        let (ws_tx, ws_rx) = channel::unbounded::<Message>();

        // Channel for Alma events (message_added, etc.)
        let (event_tx, event_rx) = channel::unbounded::<AlmaEvent>();

        // Pending generations map
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));

        // Per-thread generation guards
        let guards: GenerationGuards = Arc::new(Mutex::new(HashMap::new()));
        let generation_seq = Arc::new(AtomicU64::new(1));
        let bridge_suppressions: BridgeSuppressions = Arc::new(Mutex::new(HashMap::new()));
        let bridge_owned_assistant_messages: BridgeOwnedAssistantMessages =
            Arc::new(Mutex::new(HashMap::new()));
        let connected = Arc::new(AtomicBool::new(true));

        SmolRuntime::default().spawn(connection_supervisor(
            url,
            Some(ws_stream),
            ws_rx,
            pending.clone(),
            event_tx,
            bridge_suppressions.clone(),
            bridge_owned_assistant_messages.clone(),
            connected.clone(),
        ));

        Ok(AlmaWsClient {
            ws_tx,
            pending,
            event_rx: Arc::new(Mutex::new(event_rx)),
            guards,
            generation_seq,
            bridge_suppressions,
            connected,
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
    #[allow(clippy::too_many_arguments)]
    pub async fn generate(
        &self,
        thread_id: &str,
        model: Option<&str>,
        message: &str,
        file_parts: Vec<serde_json::Value>,
        timeout_secs: u64,
        source: &str,
        ephemeral_context: &str,
    ) -> GenerationResult {
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
        if !self.is_connected() {
            return Err("Alma WebSocket connection is closed".to_string());
        }

        let (tx, rx) = channel::bounded(1);
        let generation_id = self.generation_seq.fetch_add(1, Ordering::Relaxed);

        // Register this generation request
        {
            let mut map = self.pending.lock().await;
            if let Some(existing) = map.remove(thread_id) {
                warn!(
                    "[AlmaWS] Overwriting existing pending generation for thread {} — previous generation was not resolved",
                    thread_id
                );
                let _ = existing.result_tx.try_send(Err(format!(
                    "Generation for thread {} was replaced by a newer request",
                    thread_id
                )));
            }
            map.insert(
                thread_id.to_string(),
                PendingGeneration {
                    generation_id,
                    text: String::new(),
                    user_message_id: None,
                    empty_response_grace_started: false,
                    result_tx: tx,
                },
            );
            debug!(
                "[AlmaWS] Registered pending generation {} for thread {} (pending count: {})",
                generation_id,
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

        if let Err(e) = self
            .ws_tx
            .send(Message::Text(request.to_string().into()))
            .await
        {
            remove_pending_generation_if_matches(&self.pending, thread_id, generation_id).await;
            return Err(format!("WebSocket send failed: {}", e));
        }

        debug!("[AlmaWS] generate_response sent, awaiting result...");
        remember_bridge_activity(
            &self.bridge_suppressions,
            thread_id,
            Duration::from_secs(timeout_secs)
                .saturating_add(STOP_GENERATION_DRAIN_TIMEOUT)
                .saturating_add(RECENT_BRIDGE_GENERATION_TTL),
        )
        .await;

        // Wait for the response with timeout
        let generation_result = match SmolRuntime::default()
            .timeout(Duration::from_secs(timeout_secs), rx.recv())
            .await
        {
            Some(Ok(result)) => {
                remember_bridge_activity(
                    &self.bridge_suppressions,
                    thread_id,
                    RECENT_BRIDGE_GENERATION_TTL,
                )
                .await;
                result
            }
            Some(Err(_)) => {
                remove_pending_generation_if_matches(&self.pending, thread_id, generation_id).await;
                Err("Generation channel closed unexpectedly".to_string())
            }
            None => {
                warn!(
                    "[AlmaWS] Generation {} for thread {} timed out after {}s; sending stop_generation before retry",
                    generation_id, thread_id, timeout_secs
                );
                request_stop_generation(&self.ws_tx, thread_id);
                remember_bridge_activity(
                    &self.bridge_suppressions,
                    thread_id,
                    STALE_BRIDGE_GENERATION_SUPPRESSION_TTL,
                )
                .await;
                if wait_for_generation_drain(&self.pending, thread_id, generation_id).await {
                    info!(
                        "[AlmaWS] Stale generation {} for thread {} drained after stop_generation",
                        generation_id, thread_id
                    );
                } else {
                    warn!(
                        "[AlmaWS] Stale generation {} for thread {} did not drain after stop_generation; dropping local pending slot",
                        generation_id, thread_id
                    );
                    remove_pending_generation_if_matches(&self.pending, thread_id, generation_id)
                        .await;
                }
                Err(format!("Generation timed out after {}s", timeout_secs))
            }
        };

        drop(_guard_lock);
        prune_generation_guard(&self.guards, thread_id, &guard).await;
        generation_result
    }

    /// Receive the next Alma event (non-blocking).
    /// Returns None if no event is available right now.
    pub async fn try_recv_event(&self) -> Option<AlmaEvent> {
        let rx = self.event_rx.lock().await;
        rx.try_recv().ok()
    }

    /// Check if the WebSocket connection is alive.
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst) && !self.ws_tx.is_closed()
    }
}

async fn connection_supervisor(
    url: String,
    mut initial_stream: Option<AlmaWsStream>,
    mut outbound_rx: channel::Receiver<Message>,
    pending: PendingMap,
    event_tx: channel::Sender<AlmaEvent>,
    bridge_suppressions: BridgeSuppressions,
    bridge_owned_assistant_messages: BridgeOwnedAssistantMessages,
    connected: Arc<AtomicBool>,
) {
    let mut reconnect_attempt: u32 = 0;

    loop {
        if outbound_rx.is_closed() && outbound_rx.is_empty() {
            connected.store(false, Ordering::SeqCst);
            debug!("[AlmaWS] outbound channel closed before reconnect");
            return;
        }

        let ws_stream = match initial_stream.take() {
            Some(stream) => stream,
            None => match async_tungstenite::smol::connect_async(&url).await {
                Ok((stream, _)) => {
                    info!("[AlmaWS] Reconnected to Alma WebSocket");
                    reconnect_attempt = 0;
                    stream
                }
                Err(e) => {
                    connected.store(false, Ordering::SeqCst);
                    reconnect_attempt = reconnect_attempt.saturating_add(1);
                    let delay = reconnect_delay_ms(reconnect_attempt);
                    warn!(
                        "[AlmaWS] Reconnect attempt {} failed: {}; retrying in {}ms",
                        reconnect_attempt, e, delay
                    );
                    if outbound_rx.is_closed() && outbound_rx.is_empty() {
                        debug!("[AlmaWS] outbound channel closed while reconnecting");
                        return;
                    }
                    SmolRuntime::default()
                        .delay(Duration::from_millis(delay))
                        .await;
                    continue;
                }
            },
        };

        connected.store(true, Ordering::SeqCst);
        if !run_connected_session(
            ws_stream,
            &mut outbound_rx,
            &pending,
            &event_tx,
            &bridge_suppressions,
            &bridge_owned_assistant_messages,
        )
        .await
        {
            connected.store(false, Ordering::SeqCst);
            break;
        }

        connected.store(false, Ordering::SeqCst);
        fail_pending_generations(&pending, "Alma WebSocket disconnected").await;
        reconnect_attempt = reconnect_attempt.saturating_add(1);
        let delay = reconnect_delay_ms(reconnect_attempt);
        warn!(
            "[AlmaWS] Connection lost; reconnecting in {}ms (attempt {})",
            delay, reconnect_attempt
        );
        SmolRuntime::default()
            .delay(Duration::from_millis(delay))
            .await;
    }
}

async fn run_connected_session(
    ws_stream: AlmaWsStream,
    outbound_rx: &mut channel::Receiver<Message>,
    pending: &PendingMap,
    event_tx: &channel::Sender<AlmaEvent>,
    bridge_suppressions: &BridgeSuppressions,
    bridge_owned_assistant_messages: &BridgeOwnedAssistantMessages,
) -> bool {
    let (mut sink, mut stream) = ws_stream.split();
    let mut generating_threads: HashSet<String> = HashSet::new();
    let mut pending_assistant_updates: HashMap<String, AlmaEvent> = HashMap::new();
    let mut recent_bridge_generations: HashMap<String, VecDeque<RecentBridgeGeneration>> =
        HashMap::new();

    loop {
        let outbound = outbound_rx.recv().fuse();
        let incoming = stream.next().fuse();
        futures_util::pin_mut!(outbound, incoming);

        select! {
            outbound = outbound => {
                let Ok(msg) = outbound else {
                    debug!("[AlmaWS] outbound channel closed");
                    return false;
                };
                if let Err(e) = sink.send(msg).await {
                    error!("Alma WS write error: {}", e);
                    return true;
                }
            }
            incoming = incoming => {
                let msg = match incoming {
                    Some(Ok(msg)) => msg,
                    Some(Err(e)) => {
                        error!("Alma WS read error: {}", e);
                        return true;
                    }
                    None => {
                        info!("Alma WebSocket stream ended");
                        return true;
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
                            pending,
                            event_tx,
                            &mut generating_threads,
                            &mut pending_assistant_updates,
                            &mut recent_bridge_generations,
                            bridge_suppressions,
                            bridge_owned_assistant_messages,
                        )
                        .await;
                    }
                    Message::Close(_) => {
                        info!("Alma WebSocket closed");
                        return true;
                    }
                    Message::Ping(payload) => {
                        debug!("Alma WS ping received; sending pong");
                        if let Err(e) = sink.send(Message::Pong(payload)).await {
                            error!("Alma WS pong send failed: {}", e);
                            return true;
                        }
                    }
                    Message::Pong(_) => {
                        debug!("Alma WS pong received");
                    }
                    Message::Binary(_) | Message::Frame(_) => {}
                }
            }
        }
    }
}

async fn fail_pending_generations(pending: &PendingMap, reason: &str) {
    let mut map = pending.lock().await;
    for (thread_id, pg) in map.drain() {
        let _ = pg.result_tx.try_send(Err(reason.to_string()));
        warn!(
            "Pending generation for thread {} failed: {}",
            thread_id, reason
        );
    }
}

fn request_stop_generation(ws_tx: &channel::Sender<Message>, thread_id: &str) {
    let request = json!({
        "type": "stop_generation",
        "data": { "threadId": thread_id }
    });
    if let Err(e) = ws_tx.try_send(Message::Text(request.to_string().into())) {
        warn!(
            "[AlmaWS] Failed to send stop_generation for thread {}: {}",
            thread_id, e
        );
    }
}

async fn wait_for_generation_drain(
    pending: &PendingMap,
    thread_id: &str,
    generation_id: u64,
) -> bool {
    let start = Instant::now();
    loop {
        {
            let map = pending.lock().await;
            let still_pending = map
                .get(thread_id)
                .map(|pg| pg.generation_id == generation_id)
                .unwrap_or(false);
            if !still_pending {
                return true;
            }
        }

        if start.elapsed() >= STOP_GENERATION_DRAIN_TIMEOUT {
            return false;
        }
        SmolRuntime::default()
            .delay(STOP_GENERATION_DRAIN_POLL)
            .await;
    }
}

async fn remove_pending_generation_if_matches(
    pending: &PendingMap,
    thread_id: &str,
    generation_id: u64,
) -> Option<PendingGeneration> {
    let mut map = pending.lock().await;
    let should_remove = map
        .get(thread_id)
        .map(|pg| pg.generation_id == generation_id)
        .unwrap_or(false);
    if should_remove {
        map.remove(thread_id)
    } else {
        None
    }
}

fn reconnect_delay_ms(attempt: u32) -> u64 {
    let exponent = attempt.saturating_sub(1).min(5);
    1_000_u64.saturating_mul(1_u64 << exponent).min(30_000)
}

async fn prune_generation_guard(
    guards: &GenerationGuards,
    thread_id: &str,
    guard: &Arc<Mutex<()>>,
) {
    let mut map = guards.lock().await;
    let should_remove = map
        .get(thread_id)
        .map(|current| Arc::ptr_eq(current, guard) && Arc::strong_count(guard) == 2)
        .unwrap_or(false);
    if should_remove {
        map.remove(thread_id);
    }
}

fn pending_generation_parts(pg: &PendingGeneration) -> (String, Option<String>, Option<String>) {
    let normalized = normalize_assistant_text(&pg.text);
    let visible_text = strip_tag_blocks(&normalized, "<system-reminder>", "</system-reminder>");
    let (clean_text, thinking) = extract_think_blocks(&visible_text);
    let trimmed = clean_text.trim().to_string();
    let thinking = thinking
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty());
    let user_msg_id = pg.user_message_id.clone();

    (trimmed, thinking, user_msg_id)
}

fn resolve_pending_generation(thread_id: &str, pg: PendingGeneration) -> Option<String> {
    let (trimmed, thinking, user_msg_id) = pending_generation_parts(&pg);

    if trimmed.is_empty() {
        let _ = pg
            .result_tx
            .try_send(Err("Empty response from Alma".to_string()));
        None
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
        let completed_text = trimmed.clone();
        let _ = pg.result_tx.try_send(Ok((trimmed, user_msg_id, thinking)));
        Some(completed_text)
    }
}

async fn resolve_empty_generation_after_grace(pending: PendingMap, thread_id: String) {
    SmolRuntime::default()
        .delay(Duration::from_millis(1_000))
        .await;

    let pending_generation = {
        let mut map = pending.lock().await;
        map.remove(&thread_id)
    };

    if let Some(pg) = pending_generation {
        let _ = resolve_pending_generation(&thread_id, pg);
    }
}

async fn complete_pending_or_start_empty_grace(
    pending: &PendingMap,
    thread_id: &str,
) -> (bool, bool, Option<String>) {
    let mut completed_generation = None;
    let mut start_empty_grace = false;
    let mut had_pending_generation = false;
    let mut map = pending.lock().await;

    if let Some(pg) = map.get_mut(thread_id) {
        had_pending_generation = true;
        let (trimmed, _, _) = pending_generation_parts(pg);

        if trimmed.is_empty() {
            if !pg.empty_response_grace_started {
                pg.empty_response_grace_started = true;
                start_empty_grace = true;
            }
        } else {
            completed_generation = map.remove(thread_id);
        }
    } else {
        debug!(
            "[AlmaWS] completion event for thread {} — no pending generation (pending keys: {:?})",
            thread_id,
            map.keys().collect::<Vec<_>>()
        );
    }
    drop(map);

    let completed_text = if let Some(pg) = completed_generation {
        resolve_pending_generation(thread_id, pg)
    } else if start_empty_grace {
        let pending = pending.clone();
        let thread_id = thread_id.to_string();
        SmolRuntime::default().spawn(async move {
            resolve_empty_generation_after_grace(pending, thread_id).await;
        });
        None
    } else {
        None
    };

    (had_pending_generation, start_empty_grace, completed_text)
}

fn remember_bridge_generation(
    recent: &mut HashMap<String, VecDeque<RecentBridgeGeneration>>,
    thread_id: &str,
    text: Option<String>,
) {
    let Some(text) = text
        .map(|text| canonical_visible_assistant_text_for_dedup(&text))
        .filter(|text| !text.is_empty())
    else {
        return;
    };

    let deque = recent.entry(thread_id.to_string()).or_default();
    deque.push_back(RecentBridgeGeneration {
        text,
        completed_at: Instant::now(),
    });
    while deque.len() > RECENT_BRIDGE_GENERATION_LIMIT {
        deque.pop_front();
    }
}

fn is_recent_bridge_generation_update(
    recent: &mut HashMap<String, VecDeque<RecentBridgeGeneration>>,
    thread_id: &str,
    text: &str,
) -> bool {
    let Some(deque) = recent.get_mut(thread_id) else {
        return false;
    };

    while let Some(front) = deque.front() {
        if front.completed_at.elapsed() > RECENT_BRIDGE_GENERATION_TTL {
            deque.pop_front();
        } else {
            break;
        }
    }

    let candidate = canonical_visible_assistant_text_for_dedup(text);
    if candidate.is_empty() {
        return false;
    }

    deque.iter().any(|generation| {
        generation.text == candidate || is_generation_text_overlap(&generation.text, &candidate)
    })
}

fn is_generation_text_overlap(completed: &str, candidate: &str) -> bool {
    let min_len = completed.len().min(candidate.len());
    min_len >= 80 && (completed.starts_with(candidate) || candidate.starts_with(completed))
}

async fn remember_bridge_activity(
    bridge_suppressions: &BridgeSuppressions,
    thread_id: &str,
    ttl: Duration,
) {
    let suppress_until = Instant::now() + ttl;
    let mut map = bridge_suppressions.lock().await;
    let entry = map.entry(thread_id.to_string()).or_insert(suppress_until);
    if *entry < suppress_until {
        *entry = suppress_until;
    }
}

async fn is_bridge_activity_suppressed(
    bridge_suppressions: &BridgeSuppressions,
    thread_id: &str,
) -> bool {
    let now = Instant::now();
    let mut map = bridge_suppressions.lock().await;
    map.retain(|_, suppress_until| *suppress_until > now);
    map.contains_key(thread_id)
}

async fn remember_bridge_owned_assistant_message(
    bridge_owned_assistant_messages: &BridgeOwnedAssistantMessages,
    message_id: &str,
) {
    if message_id.trim().is_empty() {
        return;
    }

    let expires_at = Instant::now() + BRIDGE_OWNED_ASSISTANT_MESSAGE_TTL;
    let mut map = bridge_owned_assistant_messages.lock().await;
    prune_expired_instants(&mut map);
    map.insert(message_id.to_string(), expires_at);
}

async fn is_bridge_owned_assistant_message(
    bridge_owned_assistant_messages: &BridgeOwnedAssistantMessages,
    message_id: &str,
) -> bool {
    if message_id.trim().is_empty() {
        return false;
    }

    let mut map = bridge_owned_assistant_messages.lock().await;
    prune_expired_instants(&mut map);
    map.contains_key(message_id)
}

fn prune_expired_instants(map: &mut HashMap<String, Instant>) {
    let now = Instant::now();
    map.retain(|_, expires_at| *expires_at > now);
}

async fn has_pending_generation(pending: &PendingMap, thread_id: &str) -> bool {
    pending.lock().await.contains_key(thread_id)
}

fn event_message_id(data: &Value, msg_data: Option<&Value>) -> String {
    data.get("messageId")
        .or_else(|| data.get("id"))
        .or_else(|| msg_data.and_then(|m| m.get("messageId")))
        .or_else(|| msg_data.and_then(|m| m.get("id")))
        .and_then(|i| i.as_str())
        .unwrap_or("")
        .to_string()
}

fn event_message_role(data: &Value, msg_data: Option<&Value>) -> String {
    data.get("role")
        .or_else(|| msg_data.and_then(|m| m.get("role")))
        .and_then(|r| r.as_str())
        .unwrap_or("unknown")
        .to_string()
}

fn event_message_text(data: &Value, msg_data: Option<&Value>) -> String {
    data.get("text")
        .and_then(|t| t.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| msg_data.map(extract_text_from_parts).unwrap_or_default())
}

/// Dispatch a parsed WebSocket event to pending generations and/or event channel.
async fn dispatch_event(
    msg: &Value,
    pending: &PendingMap,
    event_tx: &channel::Sender<AlmaEvent>,
    generating_threads: &mut HashSet<String>,
    pending_assistant_updates: &mut HashMap<String, AlmaEvent>,
    recent_bridge_generations: &mut HashMap<String, VecDeque<RecentBridgeGeneration>>,
    bridge_suppressions: &BridgeSuppressions,
    bridge_owned_assistant_messages: &BridgeOwnedAssistantMessages,
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

                    if delta_type == "text_append"
                        && part_type == "text"
                        && let Some(text) = delta.get("text").and_then(|t| t.as_str())
                    {
                        pg.text.push_str(text);
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
                let (had_pending_generation, _, completed_text) =
                    complete_pending_or_start_empty_grace(pending, thread_id).await;
                if had_pending_generation {
                    remember_bridge_activity(
                        bridge_suppressions,
                        thread_id,
                        RECENT_BRIDGE_GENERATION_TTL,
                    )
                    .await;
                }
                remember_bridge_generation(recent_bridge_generations, thread_id, completed_text);

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
                        let _ = event_tx.send(event).await;
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
            let had_pending_generation = if let Some(pg) = map.remove(thread_id) {
                error!(
                    "[AlmaWS] Generation error for thread {}: {}",
                    thread_id, error_msg
                );
                let _ = pg.result_tx.try_send(Err(error_msg));
                true
            } else {
                warn!(
                    "[AlmaWS] Generation error for thread {} — no pending: {}",
                    thread_id, error_msg
                );
                false
            };
            drop(map);
            if had_pending_generation {
                remember_bridge_activity(
                    bridge_suppressions,
                    thread_id,
                    STALE_BRIDGE_GENERATION_SUPPRESSION_TTL,
                )
                .await;
            }

            generating_threads.remove(thread_id);
            pending_assistant_updates.remove(thread_id);
        }

        "generation_stopped" => {
            let thread_id = match data
                .get("threadId")
                .or_else(|| data.get("id"))
                .and_then(|t| t.as_str())
            {
                Some(id) => id,
                None => return,
            };

            let mut map = pending.lock().await;
            let had_pending_generation = if let Some(pg) = map.remove(thread_id) {
                info!(
                    "[AlmaWS] Generation stopped for thread {} (generation={})",
                    thread_id, pg.generation_id
                );
                let _ = pg.result_tx.try_send(Err("Generation stopped".to_string()));
                true
            } else {
                false
            };
            drop(map);
            if had_pending_generation {
                remember_bridge_activity(
                    bridge_suppressions,
                    thread_id,
                    STALE_BRIDGE_GENERATION_SUPPRESSION_TTL,
                )
                .await;
            }

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

            let msg_data = data.get("message");
            let role = event_message_role(data, msg_data);
            let message_id = event_message_id(data, msg_data);

            // Extract text from message parts (only type:"text", skip reasoning/step-start)
            let normalized_text = normalize_assistant_text(&event_message_text(data, msg_data));
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
                if role == "assistant"
                    && has_pending_generation(pending, &thread_id).await
                    && !message_id.is_empty()
                {
                    remember_bridge_owned_assistant_message(
                        bridge_owned_assistant_messages,
                        &message_id,
                    )
                    .await;
                    debug!(
                        "[AlmaWS] message_updated: suppressing bridge-owned assistant update {} while generation is pending in thread {}",
                        message_id, thread_id
                    );
                    return;
                }

                if role == "assistant"
                    && is_bridge_owned_assistant_message(
                        bridge_owned_assistant_messages,
                        &message_id,
                    )
                    .await
                {
                    debug!(
                        "[AlmaWS] message_updated: suppressing bridge-owned assistant message {} in thread {}",
                        message_id, thread_id
                    );
                    return;
                }

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
                };

                if role == "assistant" && generating_threads.contains(&thread_id) {
                    debug!(
                        "[AlmaWS] message_updated: buffering assistant update while thread {} is generating",
                        thread_id
                    );
                    pending_assistant_updates.insert(thread_id, event);
                    return;
                }

                if role == "assistant"
                    && is_recent_bridge_generation_update(
                        recent_bridge_generations,
                        &thread_id,
                        &event.message_text,
                    )
                {
                    debug!(
                        "[AlmaWS] message_updated: suppressing post-completion update for bridge-owned generation in thread {}",
                        thread_id
                    );
                    return;
                }

                if role == "assistant"
                    && is_bridge_activity_suppressed(bridge_suppressions, &thread_id).await
                {
                    debug!(
                        "[AlmaWS] message_updated: suppressing assistant update in bridge-owned generation window for thread {}",
                        thread_id
                    );
                    return;
                }

                let _ = event_tx.send(event).await;
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

            let role = data.get("message").map_or_else(
                || event_message_role(data, None),
                |m| event_message_role(data, Some(m)),
            );

            if role == "user" {
                let msg_id = data
                    .get("messageId")
                    .or_else(|| data.get("id"))
                    .or_else(|| data.get("message").and_then(|m| m.get("messageId")))
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
            } else if role == "assistant" {
                let msg_id = event_message_id(data, data.get("message"));
                if !msg_id.is_empty() && has_pending_generation(pending, thread_id).await {
                    remember_bridge_owned_assistant_message(
                        bridge_owned_assistant_messages,
                        &msg_id,
                    )
                    .await;
                    debug!(
                        "[AlmaWS] Captured bridge-owned assistant message ID: {} for thread {}",
                        msg_id, thread_id
                    );
                }
            }
        }

        "generation_completed" => {
            if let Some(thread_id) = data.get("threadId").and_then(|t| t.as_str()) {
                debug!("[AlmaWS] Progress ({}) for thread {}", msg_type, thread_id);
                generating_threads.remove(thread_id);
                let (had_pending_generation, _, completed_text) =
                    complete_pending_or_start_empty_grace(pending, thread_id).await;
                if had_pending_generation {
                    remember_bridge_activity(
                        bridge_suppressions,
                        thread_id,
                        RECENT_BRIDGE_GENERATION_TTL,
                    )
                    .await;
                }
                remember_bridge_generation(recent_bridge_generations, thread_id, completed_text);
                if let Some(event) = pending_assistant_updates.remove(thread_id) {
                    if had_pending_generation {
                        debug!(
                            "[AlmaWS] Discarding buffered assistant update for bridge-owned generation in thread {}",
                            thread_id
                        );
                    } else {
                        let _ = event_tx.send(event).await;
                    }
                }
            }
        }

        // Progress events — just log at debug level
        "tool_analysis_progress" | "memory_retrieval_progress" | "skill_analysis_progress" => {
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
        if ptype == "text"
            && let Some(t) = part.get("text").and_then(|t| t.as_str())
        {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(t);
        }
    }
    text
}

/// Strip `<think>...</think>` and `<thinking>...</thinking>` blocks from text.
/// Returns `(clean_text, thinking_content)`.
/// If multiple think blocks exist, their contents are joined with newlines.
pub(crate) fn extract_think_blocks(text: &str) -> (String, Option<String>) {
    // Hex-escape the `i` so repository tooling and model UIs do not interpret
    // these literal tags as actual hidden reasoning delimiters.
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
                    if b <= a {
                        Some((THINKING_CLOSE, THINKING_OPEN, b))
                    } else {
                        Some((THINK_CLOSE, THINK_OPEN, a))
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

pub(crate) fn canonical_visible_assistant_text_for_dedup(text: &str) -> String {
    sanitize_visible_assistant_text(text)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn visible_assistant_text_matches_for_dedup(left: &str, right: &str) -> bool {
    if left == right {
        return true;
    }

    let left = canonical_visible_assistant_text_for_dedup(left);
    let right = canonical_visible_assistant_text_for_dedup(right);
    !left.is_empty() && left == right
}

#[cfg(test)]
mod tests {
    use super::{
        PendingGeneration, PendingMap, dispatch_event, extract_think_blocks,
        normalize_assistant_text, sanitize_visible_assistant_text,
    };
    use futures_util::StreamExt;
    use macro_rules_attribute::apply;
    use serde_json::{Value, json};
    use smol::channel;
    use smol::lock::Mutex;
    use smol::net::TcpListener;
    use smol_macros::test;
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;
    use std::time::Duration;
    use trillium_smol::SmolRuntime;
    use trillium_websockets::async_tungstenite::{accept_async, tungstenite::Message};

    fn bridge_suppressions() -> super::BridgeSuppressions {
        Arc::new(Mutex::new(HashMap::new()))
    }

    fn bridge_owned_assistant_messages() -> super::BridgeOwnedAssistantMessages {
        Arc::new(Mutex::new(HashMap::new()))
    }

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
    fn thinking_tag_is_not_parsed_as_short_think_tag() {
        let (clean, thinking) = extract_think_blocks("<thinking>hidden</thinking>visible");

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

    #[apply(test!)]
    async fn generation_error_after_generating_false_keeps_real_error() {
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let (result_tx, result_rx) = channel::bounded(1);
        pending.lock().await.insert(
            "thread-1".to_string(),
            PendingGeneration {
                generation_id: 1,
                text: String::new(),
                user_message_id: Some("user-msg-1".to_string()),
                empty_response_grace_started: false,
                result_tx,
            },
        );

        let (event_tx, _event_rx) = channel::unbounded();
        let mut generating_threads = HashSet::from(["thread-1".to_string()]);
        let mut pending_assistant_updates = HashMap::new();
        let mut recent_bridge_generations = HashMap::new();
        let bridge_suppressions = bridge_suppressions();
        let bridge_owned_assistant_messages = bridge_owned_assistant_messages();

        dispatch_event(
            &json!({
                "type": "thread_generating",
                "data": {"id": "thread-1", "isGenerating": false}
            }),
            &pending,
            &event_tx,
            &mut generating_threads,
            &mut pending_assistant_updates,
            &mut recent_bridge_generations,
            &bridge_suppressions,
            &bridge_owned_assistant_messages,
        )
        .await;

        assert!(pending.lock().await.contains_key("thread-1"));

        dispatch_event(
            &json!({
                "type": "generation_error",
                "data": {"threadId": "thread-1", "error": "real alma error"}
            }),
            &pending,
            &event_tx,
            &mut generating_threads,
            &mut pending_assistant_updates,
            &mut recent_bridge_generations,
            &bridge_suppressions,
            &bridge_owned_assistant_messages,
        )
        .await;

        let result = SmolRuntime::default()
            .timeout(Duration::from_secs(1), result_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.unwrap_err(), "real alma error");
        assert!(!pending.lock().await.contains_key("thread-1"));
    }

    #[apply(test!)]
    async fn generation_completed_resolves_pending_text_without_generating_false() {
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let (result_tx, result_rx) = channel::bounded(1);
        pending.lock().await.insert(
            "thread-2".to_string(),
            PendingGeneration {
                generation_id: 2,
                text: "hello".to_string(),
                user_message_id: Some("user-msg-2".to_string()),
                empty_response_grace_started: false,
                result_tx,
            },
        );

        let (event_tx, _event_rx) = channel::unbounded();
        let mut generating_threads = HashSet::from(["thread-2".to_string()]);
        let mut pending_assistant_updates = HashMap::new();
        let mut recent_bridge_generations = HashMap::new();
        let bridge_suppressions = bridge_suppressions();
        let bridge_owned_assistant_messages = bridge_owned_assistant_messages();

        dispatch_event(
            &json!({
                "type": "generation_completed",
                "data": {"threadId": "thread-2"}
            }),
            &pending,
            &event_tx,
            &mut generating_threads,
            &mut pending_assistant_updates,
            &mut recent_bridge_generations,
            &bridge_suppressions,
            &bridge_owned_assistant_messages,
        )
        .await;

        let result = SmolRuntime::default()
            .timeout(Duration::from_secs(1), result_rx.recv())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(result.0, "hello");
        assert_eq!(result.1.as_deref(), Some("user-msg-2"));
        assert!(!pending.lock().await.contains_key("thread-2"));
        assert!(!generating_threads.contains("thread-2"));
    }

    #[apply(test!)]
    async fn post_completion_message_updated_for_bridge_generation_is_suppressed() {
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let (result_tx, result_rx) = channel::bounded(1);
        pending.lock().await.insert(
            "thread-3".to_string(),
            PendingGeneration {
                generation_id: 3,
                text: "同一条回复".to_string(),
                user_message_id: Some("user-msg-3".to_string()),
                empty_response_grace_started: false,
                result_tx,
            },
        );

        let (event_tx, event_rx) = channel::unbounded();
        let mut generating_threads = HashSet::from(["thread-3".to_string()]);
        let mut pending_assistant_updates = HashMap::new();
        let mut recent_bridge_generations = HashMap::new();
        let bridge_suppressions = bridge_suppressions();
        let bridge_owned_assistant_messages = bridge_owned_assistant_messages();

        dispatch_event(
            &json!({
                "type": "thread_generating",
                "data": {"id": "thread-3", "isGenerating": false}
            }),
            &pending,
            &event_tx,
            &mut generating_threads,
            &mut pending_assistant_updates,
            &mut recent_bridge_generations,
            &bridge_suppressions,
            &bridge_owned_assistant_messages,
        )
        .await;

        let result = SmolRuntime::default()
            .timeout(Duration::from_secs(1), result_rx.recv())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(result.0, "同一条回复");

        dispatch_event(
            &json!({
                "type": "message_updated",
                "data": {
                    "threadId": "thread-3",
                    "id": "assistant-msg-3",
                    "message": {
                        "id": "assistant-msg-3",
                        "role": "assistant",
                        "parts": [{"type": "text", "text": "同一条回复"}]
                    }
                }
            }),
            &pending,
            &event_tx,
            &mut generating_threads,
            &mut pending_assistant_updates,
            &mut recent_bridge_generations,
            &bridge_suppressions,
            &bridge_owned_assistant_messages,
        )
        .await;

        assert!(event_rx.try_recv().is_err());
    }

    #[apply(test!)]
    async fn post_completion_message_updated_for_bridge_generation_is_suppressed_after_break_normalize()
     {
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let (result_tx, result_rx) = channel::bounded(1);
        pending.lock().await.insert(
            "thread-4".to_string(),
            PendingGeneration {
                generation_id: 4,
                text: "段落一\n\n段落二".to_string(),
                user_message_id: Some("user-msg-4".to_string()),
                empty_response_grace_started: false,
                result_tx,
            },
        );

        let (event_tx, event_rx) = channel::unbounded();
        let mut generating_threads = HashSet::from(["thread-4".to_string()]);
        let mut pending_assistant_updates = HashMap::new();
        let mut recent_bridge_generations = HashMap::new();
        let bridge_suppressions = bridge_suppressions();
        let bridge_owned_assistant_messages = bridge_owned_assistant_messages();

        dispatch_event(
            &json!({
                "type": "generation_completed",
                "data": {"threadId": "thread-4"}
            }),
            &pending,
            &event_tx,
            &mut generating_threads,
            &mut pending_assistant_updates,
            &mut recent_bridge_generations,
            &bridge_suppressions,
            &bridge_owned_assistant_messages,
        )
        .await;

        let result = SmolRuntime::default()
            .timeout(Duration::from_secs(1), result_rx.recv())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(result.0, "段落一\n\n段落二");

        dispatch_event(
            &json!({
                "type": "message_updated",
                "data": {
                    "threadId": "thread-4",
                    "id": "assistant-msg-4",
                    "message": {
                        "id": "assistant-msg-4",
                        "role": "assistant",
                        "parts": [{"type": "text", "text": "段落一<br/>段落二"}]
                    }
                }
            }),
            &pending,
            &event_tx,
            &mut generating_threads,
            &mut pending_assistant_updates,
            &mut recent_bridge_generations,
            &bridge_suppressions,
            &bridge_owned_assistant_messages,
        )
        .await;

        assert!(event_rx.try_recv().is_err());
    }

    #[apply(test!)]
    async fn post_completion_incremental_message_updated_prefix_is_suppressed() {
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let (result_tx, result_rx) = channel::bounded(1);
        let completed = "这是同一轮桥接生成的完整回复，长度足够超过前缀抑制阈值，用来模拟 Alma 在完成后继续发出递增 message_updated 的情况。最后一句是完整内容。";
        let prefix = "这是同一轮桥接生成的完整回复，长度足够超过前缀抑制阈值，用来模拟 Alma 在完成后继续发出递增 message_updated 的情况。";
        pending.lock().await.insert(
            "thread-5".to_string(),
            PendingGeneration {
                generation_id: 5,
                text: completed.to_string(),
                user_message_id: Some("user-msg-5".to_string()),
                empty_response_grace_started: false,
                result_tx,
            },
        );

        let (event_tx, event_rx) = channel::unbounded();
        let mut generating_threads = HashSet::from(["thread-5".to_string()]);
        let mut pending_assistant_updates = HashMap::new();
        let mut recent_bridge_generations = HashMap::new();
        let bridge_suppressions = bridge_suppressions();
        let bridge_owned_assistant_messages = bridge_owned_assistant_messages();

        dispatch_event(
            &json!({
                "type": "generation_completed",
                "data": {"threadId": "thread-5"}
            }),
            &pending,
            &event_tx,
            &mut generating_threads,
            &mut pending_assistant_updates,
            &mut recent_bridge_generations,
            &bridge_suppressions,
            &bridge_owned_assistant_messages,
        )
        .await;

        let result = SmolRuntime::default()
            .timeout(Duration::from_secs(1), result_rx.recv())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(result.0, completed);

        dispatch_event(
            &json!({
                "type": "message_updated",
                "data": {
                    "threadId": "thread-5",
                    "id": "assistant-msg-5",
                    "message": {
                        "id": "assistant-msg-5",
                        "role": "assistant",
                        "parts": [{"type": "text", "text": prefix}]
                    }
                }
            }),
            &pending,
            &event_tx,
            &mut generating_threads,
            &mut pending_assistant_updates,
            &mut recent_bridge_generations,
            &bridge_suppressions,
            &bridge_owned_assistant_messages,
        )
        .await;

        assert!(event_rx.try_recv().is_err());
    }

    #[apply(test!)]
    async fn late_bridge_owned_message_update_is_suppressed_after_pending_is_gone() {
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let (result_tx, _result_rx) = channel::bounded(1);
        pending.lock().await.insert(
            "thread-late".to_string(),
            PendingGeneration {
                generation_id: 6,
                text: String::new(),
                user_message_id: Some("user-msg-late".to_string()),
                empty_response_grace_started: false,
                result_tx,
            },
        );

        let (event_tx, event_rx) = channel::unbounded();
        let mut generating_threads = HashSet::from(["thread-late".to_string()]);
        let mut pending_assistant_updates = HashMap::new();
        let mut recent_bridge_generations = HashMap::new();
        let bridge_suppressions = bridge_suppressions();
        let bridge_owned_assistant_messages = bridge_owned_assistant_messages();

        dispatch_event(
            &json!({
                "type": "message_added",
                "data": {
                    "threadId": "thread-late",
                    "messageId": "assistant-late",
                    "role": "assistant",
                    "text": ""
                }
            }),
            &pending,
            &event_tx,
            &mut generating_threads,
            &mut pending_assistant_updates,
            &mut recent_bridge_generations,
            &bridge_suppressions,
            &bridge_owned_assistant_messages,
        )
        .await;

        pending.lock().await.remove("thread-late");
        generating_threads.remove("thread-late");

        dispatch_event(
            &json!({
                "type": "message_updated",
                "data": {
                    "threadId": "thread-late",
                    "messageId": "assistant-late",
                    "role": "assistant",
                    "text": "this stale first attempt must not reach QQ"
                }
            }),
            &pending,
            &event_tx,
            &mut generating_threads,
            &mut pending_assistant_updates,
            &mut recent_bridge_generations,
            &bridge_suppressions,
            &bridge_owned_assistant_messages,
        )
        .await;

        assert!(event_rx.try_recv().is_err());
    }

    #[apply(test!)]
    async fn unmarked_top_level_message_updated_still_flows_for_gui_forwarding() {
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let (event_tx, event_rx) = channel::unbounded();
        let mut generating_threads = HashSet::new();
        let mut pending_assistant_updates = HashMap::new();
        let mut recent_bridge_generations = HashMap::new();
        let bridge_suppressions = bridge_suppressions();
        let bridge_owned_assistant_messages = bridge_owned_assistant_messages();

        dispatch_event(
            &json!({
                "type": "message_updated",
                "data": {
                    "threadId": "thread-gui",
                    "messageId": "assistant-gui",
                    "role": "assistant",
                    "text": "manual Alma GUI reply"
                }
            }),
            &pending,
            &event_tx,
            &mut generating_threads,
            &mut pending_assistant_updates,
            &mut recent_bridge_generations,
            &bridge_suppressions,
            &bridge_owned_assistant_messages,
        )
        .await;

        let event = event_rx.try_recv().expect("GUI message should flow");
        assert_eq!(event.thread_id, "thread-gui");
        assert_eq!(event.message_text, "manual Alma GUI reply");
    }

    #[apply(test!)]
    async fn timed_out_generation_sends_stop_and_suppresses_late_update() {
        let reserved = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = reserved.local_addr().unwrap();
        drop(reserved);

        let listener = TcpListener::bind(addr).await.unwrap();
        let server = smol::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = accept_async(stream).await.unwrap();

            let first = ws.next().await.unwrap().unwrap();
            let first_json: Value = serde_json::from_str(first.to_text().unwrap()).unwrap();
            assert_eq!(
                first_json.get("type").and_then(|v| v.as_str()),
                Some("generate_response")
            );

            let stop = ws.next().await.unwrap().unwrap();
            let stop_json: Value = serde_json::from_str(stop.to_text().unwrap()).unwrap();
            assert_eq!(
                stop_json.get("type").and_then(|v| v.as_str()),
                Some("stop_generation")
            );
            assert_eq!(
                stop_json
                    .get("data")
                    .and_then(|d| d.get("threadId"))
                    .and_then(|v| v.as_str()),
                Some("thread-timeout")
            );

            ws.send(Message::Text(
                json!({
                    "type": "generation_stopped",
                    "data": {"threadId": "thread-timeout"}
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
            ws.send(Message::Text(
                json!({
                    "type": "message_updated",
                    "data": {
                        "threadId": "thread-timeout",
                        "id": "late-assistant",
                        "message": {
                            "id": "late-assistant",
                            "role": "assistant",
                            "parts": [{"type": "text", "text": "late stale response"}]
                        }
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        });

        let client = super::AlmaWsClient::connect(&format!("http://{}", addr))
            .await
            .unwrap();
        let result = client
            .generate(
                "thread-timeout",
                None,
                "hello",
                Vec::new(),
                1,
                "telegram",
                "",
            )
            .await;

        assert_eq!(result.unwrap_err(), "Generation timed out after 1s");
        SmolRuntime::default()
            .timeout(Duration::from_secs(2), server)
            .await
            .unwrap();
        SmolRuntime::default()
            .delay(Duration::from_millis(100))
            .await;
        assert!(client.try_recv_event().await.is_none());
    }

    #[apply(test!)]
    async fn reconnect_keeps_outbound_messages_queued() {
        let reserved = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = reserved.local_addr().unwrap();
        drop(reserved);

        let url = format!("ws://{}", addr);
        let (outbound_tx, outbound_rx) = channel::unbounded();
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let (event_tx, _event_rx) = channel::unbounded();
        let connected = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let bridge_suppressions = bridge_suppressions();
        let bridge_owned_assistant_messages = bridge_owned_assistant_messages();

        let supervisor = smol::spawn(super::connection_supervisor(
            url,
            None,
            outbound_rx,
            pending,
            event_tx,
            bridge_suppressions,
            bridge_owned_assistant_messages,
            connected,
        ));

        SmolRuntime::default()
            .delay(Duration::from_millis(100))
            .await;
        outbound_tx
            .send(Message::Text("queued while reconnecting".into()))
            .await
            .unwrap();

        let listener = TcpListener::bind(addr).await.unwrap();
        let received = SmolRuntime::default()
            .timeout(Duration::from_secs(4), async {
                let (stream, _) = listener.accept().await.unwrap();
                let mut ws = accept_async(stream).await.unwrap();
                ws.next().await.unwrap().unwrap()
            })
            .await
            .unwrap();

        assert_eq!(received.to_text().unwrap(), "queued while reconnecting");
        drop(outbound_tx);
        supervisor.cancel().await;
    }

    /// Regression: when the `AlmaWsClient` holder is dropped, the outbound
    /// channel closes and the supervisor must stop — not spin forever trying
    /// to reconnect to an Alma server it can no longer serve. This exercises
    /// the exact "connect fails while outbound channel is closed" branch.
    #[apply(test!)]
    async fn supervisor_exits_when_outbound_channel_closed_before_connect() {
        // Bind + immediately drop so connect_async() to this address fails.
        let reserved = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = reserved.local_addr().unwrap();
        drop(reserved);

        let url = format!("ws://{}", addr);
        let (_outbound_tx, outbound_rx) = channel::unbounded::<Message>();
        // Close the channel immediately — simulates the last AlmaWsClient clone
        // being dropped before the supervisor ever establishes a session.
        drop(_outbound_tx);

        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let (event_tx, _event_rx) = channel::unbounded();
        let connected = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let bridge_suppressions = bridge_suppressions();
        let bridge_owned_assistant_messages = bridge_owned_assistant_messages();

        let supervisor = smol::spawn(super::connection_supervisor(
            url,
            None,
            outbound_rx,
            pending,
            event_tx,
            bridge_suppressions,
            bridge_owned_assistant_messages,
            connected,
        ));

        // If the supervisor were to spin (the reviewed bug), this would hang
        // until the test timeout. Instead it should return promptly.
        SmolRuntime::default()
            .timeout(Duration::from_secs(2), supervisor)
            .await
            .expect("supervisor did not exit after outbound channel was closed");
    }
}
