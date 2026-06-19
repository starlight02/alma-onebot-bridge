use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use warp::ws::Message;

use crate::alma;
use crate::alma_ws::AlmaEvent;
use crate::onebot::event::{
    OneBotEvent, contains_at_bot, convert_faces_to_text, extract_forward_id, extract_images,
    extract_media_summary, extract_reply_id, extract_text, has_media_segments,
};
use crate::onebot::{
    PendingCalls, get_forward_msg, get_msg, send_reply_message, send_text_message,
};
use crate::people::ensure_people_profile;
use crate::state::SharedState;
use serde_json;

const QQ_MSG_LIMIT: usize = 4500;
const ALMA_ERROR_REPLY: &str = "抱歉，我暂时无法回复 >_<";

/// Process a `post_type=message` event through the full pipeline:
///
/// 1. Extract text from message segments
/// 2. Filter group messages (require @bot)
/// 3. Ensure People Profile exists
/// 4. Get or create Alma Thread
/// 5. Send via Alma WebSocket (full chat pipeline: SOUL + Memory + People Profiles)
/// 6. Split reply into paragraphs and send each as a separate QQ message
pub async fn process_message_event(
    event: &OneBotEvent,
    state: &SharedState,
    ws_tx: &mpsc::UnboundedSender<Message>,
    pending: &PendingCalls,
) -> Result<(), String> {
    let message_type = event.message_type.as_deref().unwrap_or("unknown");
    let segments = event.message.as_deref().unwrap_or(&[]);

    // ── Extract plain text + face emojis ────────────────────────────────────
    let text = extract_text(segments);
    let face_text = convert_faces_to_text(segments);

    // Combine text and face content
    let text = if face_text.is_empty() {
        text
    } else if text.is_empty() {
        face_text
    } else {
        format!("{} {}", text, face_text)
    };

    // Extract media info (images, voice, video, etc.)
    let image_urls = extract_images(segments);
    let media_lines = extract_media_summary(segments);
    let has_media = has_media_segments(segments);

    if text.is_empty() && !has_media {
        info!(
            "[Message] No text or media from {:?}, skipping",
            event.user_id
        );
        return Ok(());
    }

    // ── Identify sender and chat context ─────────────────────────────────
    let bot_id = event.self_id;
    let is_group = message_type == "group";
    let user_id = event.user_id.unwrap_or(0);
    let group_id = event.group_id.unwrap_or(0);
    let sender_nickname = event
        .sender
        .as_ref()
        .and_then(|s| s.nickname.as_deref())
        .unwrap_or("unknown");

    // For group chats, prefer the group card (群名片) over the QQ nickname
    let display_name = if is_group {
        event
            .sender
            .as_ref()
            .and_then(|s| s.card.as_deref())
            .filter(|c| !c.is_empty())
            .unwrap_or(sender_nickname)
    } else {
        sender_nickname
    };

    // ── Record to group history (before @bot gate, so ALL messages are captured) ──
    if is_group && !text.is_empty() {
        let session_key = format!("group:{}", group_id);
        state
            .record_group_message(
                &session_key,
                crate::state::GroupMessage {
                    display_name: display_name.to_string(),
                    text: text.clone(),
                    timestamp: event.time.unwrap_or(0),
                },
            )
            .await;
        debug!(
            "[GroupHistory] Recorded message from {} in {}",
            display_name, session_key
        );
    }

    // ── Group message: require @bot ──────────────────────────────────────
    let cleaned_text = if is_group {
        if !contains_at_bot(segments, bot_id) {
            return Ok(());
        }
        crate::onebot::event::clean_at_from_text(&text, bot_id)
    } else {
        text.clone()
    };

    let source = if is_group {
        format!("群{}", group_id)
    } else {
        "私聊".to_string()
    };
    info!("[Message] {} {}: {}", source, display_name, cleaned_text);

    // ── Message ID & Reply context ────────────────────────────────────────
    // Use real OneBot message_id for [msg:N] (matches Telegram bridge pattern)
    let message_id = event.message_id.unwrap_or(0);

    // Handle reply/quoting: extract reply segment and fetch quoted message
    let reply_context = if let Some(reply_msg_id) = extract_reply_id(segments) {
        match get_msg(
            ws_tx,
            pending,
            &reply_msg_id,
            state.config.onebot_api_timeout_secs,
        )
        .await
        {
            Ok((sender_name, quoted_text)) => {
                let truncated = if quoted_text.len() > 200 {
                    format!("{}...", &quoted_text[..200])
                } else {
                    quoted_text
                };
                info!(
                    "[Reply] Quoting {}'s message: \"{}\"",
                    sender_name, truncated
                );
                Some(format!(
                    "[Replying to {}'s message: \"{}\"]",
                    sender_name, truncated
                ))
            }
            Err(e) => {
                tracing::debug!("get_msg failed for reply context: {}", e);
                None
            }
        }
    } else {
        None
    };

    // ── Forwarded message content ─────────────────────────────────────────
    // If the message contains a forward segment, fetch the forwarded content
    let forward_context = if let Some(forward_id) = extract_forward_id(segments) {
        match get_forward_msg(
            ws_tx,
            pending,
            &forward_id,
            state.config.onebot_api_timeout_secs,
        )
        .await
        {
            Ok(nodes) if !nodes.is_empty() => {
                let summaries: Vec<String> = nodes
                    .iter()
                    .take(20) // Limit to first 20 nodes to avoid huge messages
                    .map(|(name, text)| {
                        let truncated = if text.len() > 100 {
                            format!("{}...", &text[..100])
                        } else {
                            text.clone()
                        };
                        format!("{}: \"{}\"", name, truncated)
                    })
                    .collect();
                let count = nodes.len();
                let suffix = if count > 20 {
                    format!(" ... +{} more", count - 20)
                } else {
                    String::new()
                };
                info!("[Forward] Extracted {} nodes from forwarded message", count);
                Some(format!(
                    "[Forwarded messages ({} total):{}{}]",
                    count,
                    summaries.join(", "),
                    suffix
                ))
            }
            Ok(_) => {
                info!("[Forward] Forwarded message was empty");
                Some("[Forwarded message]".to_string())
            }
            Err(e) => {
                tracing::debug!("get_forward_msg failed: {}", e);
                Some("[Forwarded message]".to_string())
            }
        }
    } else {
        None
    };

    // ── Ensure People Profile ────────────────────────────────────────────
    ensure_people_profile(user_id, event.sender.as_ref(), state, ws_tx, pending).await;

    // ── Session key & Alma thread ────────────────────────────────────────
    let session_key = if is_group {
        format!("group:{}", group_id)
    } else {
        format!("private:{}", user_id)
    };

    let (thread_id, has_existing_thread) = match state.get_thread_id(&session_key).await {
        Some(tid) => (tid, true),
        None => {
            let title = if is_group {
                format!("QQ群 {}", group_id)
            } else {
                format!("QQ私聊 {}", sender_nickname)
            };
            let tid = alma::create_thread(state, &title).await?;
            info!("[Thread] Created: '{}' → {}", title, tid);
            state.set_thread_id(session_key, tid.clone()).await;
            (tid, false)
        }
    };

    // ── Call Alma AI via WebSocket (full chat pipeline) ──────────────────
    let alma_ws = match state.get_alma_ws().await {
        Some(ws) => ws,
        None => {
            warn!("[Alma] WebSocket client not connected");
            let target_id = if is_group { group_id } else { user_id };
            let target_type = if is_group { "group" } else { "private" };
            let _ = send_text_message(
                ws_tx,
                pending,
                target_type,
                target_id,
                ALMA_ERROR_REPLY,
                state.config.onebot_api_timeout_secs,
            )
            .await;
            return Err("Alma WebSocket not connected".to_string());
        }
    };

    let fallback_model = state
        .get_default_model()
        .await
        .unwrap_or_else(|| "anthropic:claude-sonnet-4-20250514".to_string());

    let model = if has_existing_thread {
        match alma::fetch_thread_model(state, &thread_id).await {
            Ok(Some(model)) => {
                info!(
                    "[Alma] Existing thread {} currently reports model {} — omitting model field and letting Alma use thread state",
                    thread_id, model
                );
            }
            Ok(None) => {
                info!(
                    "[Alma] Existing thread {} has no reported model — omitting model field and letting Alma choose",
                    thread_id
                );
            }
            Err(e) => {
                warn!(
                    "[Alma] Failed to fetch model for thread {}: {} — omitting model field and letting Alma choose",
                    thread_id, e
                );
            }
        }
        None
    } else {
        info!(
            "[Alma] Using bootstrap model for new thread {}: {}",
            thread_id, fallback_model
        );
        Some(fallback_model)
    };

    // ── Telegram-style message format (for Alma channel protocol) ─────────
    // Format: "[From: DisplayName | id:qq_id]\n\n[msg:N] message text"
    // [msg:N] uses real OneBot message_id (matches Telegram bridge pattern)
    // Forward context (if present) describes forwarded message content
    // Reply context (if present) describes quoted message
    // Media info (images, voice, video) appended as additional lines
    let text_with_context = {
        let mut parts = Vec::new();
        if let Some(ref fwd) = forward_context {
            parts.push(fwd.clone());
        }
        if let Some(ref ctx) = reply_context {
            parts.push(ctx.clone());
        }
        parts.push(cleaned_text.clone());
        parts.join("\n")
    };

    // ── Download images and file attachments ───────────────────────────────
    let mut file_parts = Vec::new();

    // Download images
    for (idx, url) in image_urls.iter().enumerate() {
        let default_filename = format!("image_{}.png", idx + 1);
        match download_media_as_file_part(&state.http_client, url, &default_filename).await {
            Ok(part) => {
                info!(
                    "[Alma] Successfully downloaded and prepared image part: {}",
                    url
                );
                file_parts.push(part);
            }
            Err(e) => {
                warn!("[Alma] Failed to download image {}: {}", url, e);
            }
        }
    }

    // Download file attachments
    let file_attachments = crate::onebot::event::extract_files(segments);
    for (filename, url) in &file_attachments {
        match download_media_as_file_part(&state.http_client, url, filename).await {
            Ok(part) => {
                info!(
                    "[Alma] Successfully downloaded and prepared file part: {} ({})",
                    filename, url
                );
                file_parts.push(part);
            }
            Err(e) => {
                warn!(
                    "[Alma] Failed to download file {} from {}: {}",
                    filename, url, e
                );
            }
        }
    }

    // Build media suffix: only keep other media type indicators (like Voice/Video)
    // since images/files are attached natively as file parts.
    let media_suffix = {
        let mut lines = Vec::new();
        for line in &media_lines {
            if line != "[Image]" && line != "[File]" {
                lines.push(line.clone());
            }
        }
        if lines.is_empty() {
            String::new()
        } else {
            format!("\n\n{}", lines.join("\n\n"))
        }
    };

    let formatted_message = format!(
        "[From: {} | id:{}]\n\n[msg:{}] {}{}",
        display_name, user_id, message_id, text_with_context, media_suffix
    );

    // ── Source: spoof as Telegram for Alma's server-side processing ────────
    // "telegram-group" gets group chat rules, privacy firewall, history stripping
    // "telegram" gets private chat treatment
    let source = if is_group {
        "telegram-group"
    } else {
        "telegram"
    };

    // ── Build ephemeralContext with SENDER PROFILE ─────────────────────────
    let mut ephemeral_ctx = String::new();

    // Scan people profiles for the sender's qq_id
    if let Some(profile_block) = crate::people::find_sender_profile(
        &state.config.people_dir,
        &user_id.to_string(),
        display_name,
    ) {
        ephemeral_ctx.push_str(&profile_block);
    }

    // Add PEOPLE PROFILES summary line
    let profile_count = crate::people::count_profiles(&state.config.people_dir);
    if profile_count > 0 {
        ephemeral_ctx.push_str(&format!(
            "\n\nPEOPLE PROFILES — You know {} people. Use `alma people list` or `alma people show <name>` to look up profiles on demand.",
            profile_count
        ));
    }

    // Add group chat history (if available and in group context)
    if is_group {
        let history_session_key = format!("group:{}", group_id);
        let history = state.get_group_history(&history_session_key).await;
        if !history.is_empty() {
            info!(
                "[GroupHistory] Injecting {} messages into ephemeralContext for {}",
                history.len(),
                history_session_key
            );
            ephemeral_ctx.push_str(&format!(
                "\n\nRECENT GROUP CHAT HISTORY (last {} messages):",
                history.len()
            ));
            for msg in &history {
                let ts = if msg.timestamp > 0 {
                    // Format timestamp as HH:MM (UTC+8)
                    let secs = msg.timestamp % 86400;
                    let hours = (secs / 3600 + 8) % 24; // rough UTC+8
                    let minutes = (secs % 3600) / 60;
                    format!("[{:02}:{:02}] ", hours, minutes)
                } else {
                    String::new()
                };
                let truncated = if msg.text.len() > 200 {
                    format!("{}...", &msg.text[..200])
                } else {
                    msg.text.clone()
                };
                ephemeral_ctx.push_str(&format!("\n{}{}: {}", ts, msg.display_name, truncated));
            }
        }
    }

    // Log ephemeral context for debugging
    if ephemeral_ctx.is_empty() {
        debug!("[Alma] ephemeralContext is empty (no profiles, no group history)");
    } else {
        debug!(
            "[Alma] ephemeralContext ({} chars):\n{}",
            ephemeral_ctx.len(),
            ephemeral_ctx
        );
    }

    // ── Send thinking indicator (optional, config-gated) ─────────────────
    // If configured, sends a brief placeholder message before generation starts,
    // so users see activity while Alma is processing. OneBot v11 has no typing API.
    if let Some(ref thinking_msg) = state.config.thinking_message {
        let target_id = if is_group { group_id } else { user_id };
        let target_type = if is_group { "group" } else { "private" };
        match send_text_message(
            ws_tx,
            pending,
            target_type,
            target_id,
            thinking_msg,
            state.config.onebot_api_timeout_secs,
        )
        .await
        {
            Ok(_) => info!(
                "[Thinking] Sent '{}' to {} {}",
                thinking_msg, target_type, target_id
            ),
            Err(e) => tracing::debug!("[Thinking] Failed to send thinking message: {}", e),
        }
    }

    let (reply, thinking) = {
        let max_retries = state.config.alma_max_retries;
        let base_delay_ms = state.config.alma_retry_delay_ms;
        let mut last_err = String::new();

        let mut result = None;
        let mut thinking_content = None;
        for attempt in 0..=max_retries {
            if attempt > 0 {
                let delay = base_delay_ms * (1 << (attempt - 1)); // exponential backoff
                info!(
                    "[Alma] Retry {}/{} for thread {} in {}ms",
                    attempt, max_retries, thread_id, delay
                );
                tokio::time::sleep(tokio::time::Duration::from_millis(delay)).await;
            }

            match alma_ws
                .generate(
                    &thread_id,
                    model.as_deref(),
                    &formatted_message,
                    file_parts.clone(),
                    state.config.alma_run_timeout_secs,
                    source,
                    &ephemeral_ctx,
                )
                .await
            {
                Ok((r, _user_msg_id, thinking)) => {
                    result = Some(r);
                    thinking_content = thinking;
                    break;
                }
                Err(e) => {
                    warn!("[Alma] Generation attempt {} failed: {}", attempt + 1, e);
                    last_err = e;
                }
            }
        }

        match result {
            Some(r) => (r, thinking_content),
            None => {
                warn!(
                    "[Alma] Generation failed: {}",
                    last_err
                );
                (ALMA_ERROR_REPLY.to_string(), None)
            }
        }
    };

    // ── Send reply via OneBot ────────────────────────────────────────────
    if reply.is_empty() {
        info!("[Reply] Empty reply, skipping");
        return Ok(());
    }

    let target_id = if is_group { group_id } else { user_id };
    let target_type = if is_group { "group" } else { "private" };

    // ── Send thinking content as separate message (if enabled) ────────────
    if state.config.show_thinking {
        if let Some(ref think_text) = thinking {
            if !think_text.is_empty() {
                info!("[Thinking] Sending thinking content ({} chars)", think_text.len());
                let think_chunks = split_text(think_text, QQ_MSG_LIMIT);
                for chunk in &think_chunks {
                    match send_text_message(
                        ws_tx,
                        pending,
                        target_type,
                        target_id,
                        chunk,
                        state.config.onebot_api_timeout_secs,
                    )
                    .await
                    {
                        Ok(_) => {}
                        Err(e) => tracing::debug!("[Thinking] Failed to send thinking: {}", e),
                    }
                }
            }
        }
    }

    // Split by paragraphs first, then by QQ message limit
    let paragraphs = split_paragraphs(&reply);
    let user_message_id = event.message_id.map(|id| id.to_string());
    let at_user_id = if is_group {
        Some(user_id.to_string())
    } else {
        None
    };
    let mut is_first = true;
    for para in &paragraphs {
        let chunks = split_text(para, QQ_MSG_LIMIT);
        for chunk in &chunks {
            // Register this reply for dedup (bidirectional)
            state.register_sent_reply(&thread_id, chunk).await;

            // Reply to user's triggering message (first chunk only, groups + private)
            let result = if is_first {
                is_first = false;
                if let Some(ref reply_id) = user_message_id {
                    send_reply_message(
                        ws_tx,
                        pending,
                        target_type,
                        target_id,
                        chunk,
                        reply_id,
                        at_user_id.as_deref(),
                        state.config.onebot_api_timeout_secs,
                    )
                    .await
                } else {
                    send_text_message(
                        ws_tx,
                        pending,
                        target_type,
                        target_id,
                        chunk,
                        state.config.onebot_api_timeout_secs,
                    )
                    .await
                }
            } else {
                send_text_message(
                    ws_tx,
                    pending,
                    target_type,
                    target_id,
                    chunk,
                    state.config.onebot_api_timeout_secs,
                )
                .await
            };

            match result {
                Ok(resp) => {
                    let msg_id = resp
                        .data
                        .as_ref()
                        .and_then(|d| d.get("message_id"))
                        .and_then(|m| m.as_i64());
                    info!(
                        "[Reply] Sent to {} {}, msg_id={:?}",
                        target_type, target_id, msg_id
                    );
                }
                Err(e) => {
                    warn!(
                        "[Reply] Failed to send to {} {}: {}",
                        target_type, target_id, e
                    );
                }
            }
        }
    }

    Ok(())
}

/// Handle an Alma event for bidirectional forwarding (Alma GUI → QQ).
///
/// When someone types in the Alma GUI and the assistant responds,
/// forward the assistant's reply to the corresponding QQ chat.
pub async fn handle_alma_event(
    event: &AlmaEvent,
    state: &SharedState,
    ws_tx: &mpsc::UnboundedSender<Message>,
    pending: &PendingCalls,
) -> Result<(), String> {
    if event.event_type != "message_updated" || event.message_role != "assistant" {
        return Ok(());
    }

    // Only forward assistant messages from threads we're tracking
    let target = match state.get_qq_target(&event.thread_id).await {
        Some(t) => t,
        None => {
            tracing::debug!(
                "[Alma→QQ] Thread {} not tracked (no QQ target in reverse map), skipping",
                event.thread_id
            );
            return Ok(());
        }
    };

    // Dedup: skip if we already sent this text ourselves
    if state
        .was_sent_recently(&event.thread_id, &event.message_text)
        .await
    {
        tracing::debug!(
            "[Alma→QQ] Skipping duplicate reply in thread {} (text_len={})",
            event.thread_id, event.message_text.len()
        );
        return Ok(());
    }

    info!(
        "[Alma→QQ] Forwarding assistant message to {} {} (thread={}, {} chars)",
        target.target_type, target.target_id, event.thread_id, event.message_text.len()
    );

    // Forward the Alma GUI assistant message to QQ
    let chunks = split_text(&event.message_text, QQ_MSG_LIMIT);
    for chunk in &chunks {
        // Register to avoid echo loops
        state.register_sent_reply(&event.thread_id, chunk).await;

        match send_text_message(
            ws_tx,
            pending,
            &target.target_type,
            target.target_id,
            chunk,
            state.config.onebot_api_timeout_secs,
        )
        .await
        {
            Ok(resp) => {
                let msg_id = resp
                    .data
                    .as_ref()
                    .and_then(|d| d.get("message_id"))
                    .and_then(|m| m.as_i64());
                info!(
                    "[Alma→QQ] Forwarded to {} {} ({} chars, msg_id={:?})",
                    target.target_type,
                    target.target_id,
                    chunk.len(),
                    msg_id
                );
            }
            Err(e) => {
                warn!("[Alma→QQ] Failed to forward: {}", e);
            }
        }
    }

    Ok(())
}

/// Split text into paragraphs by double newlines.
/// Each paragraph becomes a separate QQ message.
fn split_paragraphs(text: &str) -> Vec<String> {
    text.split("\n\n")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Split text into chunks that fit within QQ's message length limit.
fn split_text(text: &str, limit: usize) -> Vec<String> {
    if text.len() <= limit {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut remaining = text;

    while !remaining.is_empty() {
        if remaining.len() <= limit {
            chunks.push(remaining.to_string());
            break;
        }

        let split_at = remaining[..limit].rfind('\n').unwrap_or(limit);

        chunks.push(remaining[..split_at].to_string());
        remaining = &remaining[split_at..];

        if remaining.starts_with('\n') {
            remaining = &remaining[1..];
        }
    }

    chunks
}

/// Download a media URL and encode it as a Base64 data URI in an Alma file part JSON object.
async fn download_media_as_file_part(
    client: &reqwest::Client,
    url: &str,
    default_filename: &str,
) -> Result<serde_json::Value, String> {
    use base64::prelude::*;

    debug!("[Alma] Downloading media from URL: {}", url);
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("Failed to fetch URL: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("HTTP status error: {}", resp.status()));
    }

    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            if url.ends_with(".png") {
                "image/png".to_string()
            } else if url.ends_with(".jpg") || url.ends_with(".jpeg") {
                "image/jpeg".to_string()
            } else if url.ends_with(".gif") {
                "image/gif".to_string()
            } else {
                "application/octet-stream".to_string()
            }
        });

    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("Failed to read response bytes: {}", e))?;

    let b64_data = BASE64_STANDARD.encode(&bytes);

    let filename = if let Some(last_seg) = url.split('/').last() {
        let clean_seg = last_seg.split('?').next().unwrap_or(last_seg);
        if !clean_seg.is_empty() && clean_seg.contains('.') {
            clean_seg.to_string()
        } else {
            default_filename.to_string()
        }
    } else {
        default_filename.to_string()
    };

    Ok(serde_json::json!({
        "type": "file",
        "mediaType": content_type,
        "url": format!("data:{};base64,{}", content_type, b64_data),
        "filename": filename
    }))
}
