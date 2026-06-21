use std::io::Write;

use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use warp::ws::Message;

use crate::alma;
use crate::alma_ws::{AlmaEvent, AlmaWsClient};
use crate::onebot::event::{
    OneBotEvent, contains_at_bot, convert_faces_to_text, extract_forward_id, extract_images,
    extract_media_summary, extract_reply_id, extract_text, has_media_segments,
};
use crate::onebot::{
    PendingCalls, get_forward_msg, get_group_name, get_msg, send_reply_message, send_text_message,
};
use crate::people::ensure_people_profile;
use crate::state::SharedState;
use serde_json;

const QQ_MSG_LIMIT: usize = 4500;
const ALMA_ERROR_REPLY: &str = "抱歉，我暂时无法回复 >_<";
const MAX_MEDIA_BYTES: u64 = 20 * 1024 * 1024;

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

    let group_title = if is_group {
        if let Some(title) = state.get_group_title(group_id).await {
            Some(title)
        } else {
            match get_group_name(
                ws_tx,
                pending,
                group_id,
                state.config.onebot_api_timeout_secs,
            )
            .await
            {
                Ok(Some(title)) => {
                    state.set_group_title(group_id, title.clone()).await;
                    Some(title)
                }
                Ok(None) => None,
                Err(e) => {
                    debug!("[GroupMeta] get_group_name failed for {}: {}", group_id, e);
                    None
                }
            }
        }
    } else {
        None
    };

    // ── Record to group history (before @bot gate, so ALL messages are captured) ──
    if is_group && !text.is_empty() {
        let session_key = format!("group:{}", group_id);
        let message_id = event.message_id;
        state
            .record_group_message(
                &session_key,
                crate::state::GroupMessage {
                    display_name: display_name.to_string(),
                    text: text.clone(),
                    timestamp: event.time.unwrap_or(0),
                    message_id,
                    is_bot: false,
                },
            )
            .await;
        if let Err(e) = append_to_alma_chat_log(
            group_id,
            display_name,
            &text,
            false,
            event.time.unwrap_or(0),
            message_id,
            Some(user_id),
            None,
        ) {
            debug!("[GroupHistory] Failed to append Alma chat log: {}", e);
        }
        debug!(
            "[GroupHistory] Recorded message from {} in {}",
            display_name, session_key
        );
    }

    // ── Message ID & Reply context ────────────────────────────────────────
    // Use real OneBot message_id for [msg:N] (matches Telegram bridge pattern)
    let message_id = event.message_id.unwrap_or(0);

    // Handle reply/quoting: extract reply segment and fetch quoted message
    let mut quoted_image_urls = Vec::new();
    let mut is_reply_to_bot = false;
    let reply_context = if let Some(reply_msg_id) = extract_reply_id(segments) {
        match get_msg(
            ws_tx,
            pending,
            &reply_msg_id,
            state.config.onebot_api_timeout_secs,
        )
        .await
        {
            Ok(quoted) => {
                is_reply_to_bot = quoted.sender_id == Some(bot_id);
                quoted_image_urls = quoted.image_urls.clone();
                let truncated = single_line_preview(&quoted.text, 200);
                let reply_label = if is_reply_to_bot {
                    "Alma".to_string()
                } else {
                    quoted.sender_name.clone()
                };
                info!(
                    "[Reply] Quoting {}'s message: \"{}\"",
                    reply_label, truncated
                );
                Some(if is_reply_to_bot {
                    format!("[Replying to Alma's message: \"{}\"]", truncated)
                } else {
                    format!(
                        "[Replying to {}'s message: \"{}\"]",
                        quoted.sender_name, truncated
                    )
                })
            }
            Err(e) => {
                tracing::debug!("get_msg failed for reply context: {}", e);
                None
            }
        }
    } else {
        None
    };

    // ── Group message: align trigger semantics with Telegram bridge ──────
    let should_process = if is_group {
        contains_at_bot(segments, bot_id)
            || is_reply_to_bot
            || text.to_lowercase().contains("alma")
            || cleaned_command_for_group(&text)
    } else {
        true
    };
    if !should_process {
        return Ok(());
    }

    let cleaned_text = if is_group {
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
    // Pass group_id so people.rs can record/update the group card (群名片).
    let group_id_opt = if is_group { Some(group_id) } else { None };
    ensure_people_profile(
        user_id,
        event.sender.as_ref(),
        group_id_opt,
        state,
        ws_tx,
        pending,
    )
    .await;

    // ── Session key & Alma thread ────────────────────────────────────────
    let session_key = if is_group {
        format!("group:{}", group_id)
    } else {
        format!("private:{}", user_id)
    };

    let (thread_id, has_existing_thread) =
        resolve_thread_for_session(state, &session_key, is_group, group_id, sender_nickname)
            .await?;

    // ── Call Alma AI via WebSocket (full chat pipeline) ──────────────────
    if let Err(e) = ensure_alma_ws_client(state).await {
        warn!("[Alma] WebSocket client not connected: {}", e);
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
        return Err(e);
    }

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
    // Match Alma built-in Telegram behavior as closely as possible:
    // - group messages => [From: ... [msg:N]] ...
    // - private messages => [msg:N] ...
    // - forward prefix is prepended inline
    // - reply context is prepended inline
    let text_with_context = build_telegram_like_message_text(
        is_group,
        is_reply_to_bot,
        display_name,
        None,
        event
            .sender
            .as_ref()
            .and_then(|s| s.user_id)
            .unwrap_or(user_id),
        false,
        message_id,
        cleaned_text.trim(),
        forward_context.as_deref(),
        reply_context.as_deref(),
    );

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

    // Download quoted/replied images too, so Alma can see the referenced image
    // instead of only a textual placeholder like "[Image]". This matches the
    // built-in Telegram bridge behavior more closely.
    for (idx, url) in quoted_image_urls.iter().enumerate() {
        let default_filename = format!("quoted_image_{}.png", idx + 1);
        match download_media_as_file_part(&state.http_client, url, &default_filename).await {
            Ok(part) => {
                info!(
                    "[Alma] Successfully downloaded and prepared quoted image part: {}",
                    url
                );
                file_parts.push(part);
            }
            Err(e) => {
                warn!("[Alma] Failed to download quoted image {}: {}", url, e);
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

    let formatted_message = format!("{}{}", text_with_context, media_suffix);

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
    ephemeral_ctx.push_str(&build_telegram_like_channel_system_context(
        is_group,
        sender_nickname,
        group_title.as_deref(),
        if is_group { Some(group_id) } else { None },
    ));

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
            ephemeral_ctx.push_str("\n\n");
            ephemeral_ctx.push_str(&build_recent_chat_history_context(&history));
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

            let alma_ws = match ensure_alma_ws_client(state).await {
                Ok(client) => client,
                Err(e) => {
                    warn!("[Alma] Reconnect attempt {} failed: {}", attempt + 1, e);
                    last_err = e;
                    continue;
                }
            };

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
                warn!("[Alma] Generation failed: {}", last_err);
                (ALMA_ERROR_REPLY.to_string(), None)
            }
        }
    };
    let reply = crate::alma_ws::sanitize_visible_assistant_text(&reply);
    let thinking = thinking
        .map(|t| {
            crate::alma_ws::normalize_assistant_text(&t)
                .trim()
                .to_string()
        })
        .filter(|t| !t.is_empty());

    // ── Send reply via OneBot ────────────────────────────────────────────
    if reply.is_empty() {
        info!("[Reply] Empty reply, skipping");
        return Ok(());
    }

    let target_id = if is_group { group_id } else { user_id };
    let target_type = if is_group { "group" } else { "private" };

    // Register the full normalized reply once so the later Alma
    // `message_updated` event can be deduped even if QQ delivery was split
    // into multiple chunks or paragraphs.
    state.register_sent_reply(&thread_id, &reply).await;

    // ── Send thinking content as separate message (if enabled) ────────────
    if state.config.show_thinking {
        if let Some(ref think_text) = thinking {
            if !think_text.is_empty() {
                info!(
                    "[Thinking] Sending thinking content ({} chars)",
                    think_text.len()
                );
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
                    if is_group {
                        let history_session_key = format!("group:{}", group_id);
                        state
                            .record_group_message(
                                &history_session_key,
                                crate::state::GroupMessage {
                                    display_name: "Alma".to_string(),
                                    text: chunk.to_string(),
                                    timestamp: current_unix_timestamp(),
                                    message_id: msg_id,
                                    is_bot: true,
                                },
                            )
                            .await;
                        if let Err(e) = append_to_alma_chat_log(
                            group_id,
                            "Alma",
                            chunk,
                            true,
                            current_unix_timestamp(),
                            msg_id,
                            None,
                            None,
                        ) {
                            debug!("[GroupHistory] Failed to append Alma chat log: {}", e);
                        }
                    }
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
            // Promoted to info: critical for diagnosing "Alma GUI reply not
            // synced to QQ" — usually means session_reverse cache missed and
            // DB lookup found no mapping (e.g. thread was never created via
            // bridge, or DB was reset).
            info!(
                "[Alma→QQ] Thread {} has no QQ target mapping (not in session_reverse or DB), skipping",
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
        // Promoted to info: dedup hits are the second most common reason
        // for "Alma GUI reply not synced" complaints.
        info!(
            "[Alma→QQ] Dedup hit for thread {} (prefix matches a sent reply, {} chars)",
            event.thread_id,
            event.message_text.len()
        );
        return Ok(());
    }

    info!(
        "[Alma→QQ] Forwarding assistant message to {} {} (thread={}, {} chars)",
        target.target_type,
        target.target_id,
        event.thread_id,
        event.message_text.len()
    );

    if state.config.show_thinking {
        if let Some(ref think_text) = event.thinking_text {
            if !think_text.is_empty() {
                let think_chunks = split_text(think_text, QQ_MSG_LIMIT);
                for chunk in &think_chunks {
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
                        Ok(_) => {}
                        Err(e) => tracing::debug!("[Alma→QQ] Failed to forward thinking: {}", e),
                    }
                }
            }
        }
    }

    if event.message_text.is_empty() {
        return Ok(());
    }

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
                if target.target_type == "group" {
                    let history_session_key = format!("group:{}", target.target_id);
                    state
                        .record_group_message(
                            &history_session_key,
                            crate::state::GroupMessage {
                                display_name: "Alma".to_string(),
                                text: chunk.to_string(),
                                timestamp: current_unix_timestamp(),
                                message_id: msg_id,
                                is_bot: true,
                            },
                        )
                        .await;
                    if let Err(e) = append_to_alma_chat_log(
                        target.target_id,
                        "Alma",
                        chunk,
                        true,
                        current_unix_timestamp(),
                        msg_id,
                        None,
                        None,
                    ) {
                        debug!("[GroupHistory] Failed to append Alma chat log: {}", e);
                    }
                }
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

async fn resolve_thread_for_session(
    state: &SharedState,
    session_key: &str,
    is_group: bool,
    group_id: i64,
    sender_nickname: &str,
) -> Result<(String, bool), String> {
    if let Some(thread_id) = state.get_thread_id(session_key).await {
        match alma::thread_exists(state, &thread_id).await {
            Ok(true) => return Ok((thread_id, true)),
            Ok(false) => warn!(
                "[Thread] Local mapping {} -> {} is stale; Alma API has no such thread",
                session_key, thread_id
            ),
            Err(e) => return Err(e),
        }
    }

    let title = if is_group {
        format!("QQ群 {}", group_id)
    } else {
        format!("QQ私聊 {}", sender_nickname)
    };
    let thread_id = alma::create_thread(state, &title).await?;
    info!("[Thread] Created: '{}' → {}", title, thread_id);
    state
        .set_thread_id(session_key.to_string(), thread_id.clone())
        .await;
    Ok((thread_id, false))
}

async fn ensure_alma_ws_client(state: &SharedState) -> Result<AlmaWsClient, String> {
    if let Some(client) = state.get_alma_ws().await {
        if client.is_connected() {
            return Ok(client);
        }
        warn!("[Alma] Existing WebSocket is closed; reconnecting");
    }

    let client = AlmaWsClient::connect(&state.config.alma_api).await?;
    state.set_alma_ws(client.clone()).await;
    Ok(client)
}

fn build_recent_chat_history_context(history: &[crate::state::GroupMessage]) -> String {
    let mut lines = vec![
        "RECENT CHAT HISTORY:".to_string(),
        "NOTE: Messages marked [Alma (YOU)] are YOUR OWN previous messages. When someone directs commands at [Alma (YOU)], they are talking to YOU, not your owner.".to_string(),
    ];

    for msg in history {
        let time = format_history_time(msg.timestamp);
        let message_id = msg
            .message_id
            .map(|id| format!(" [msg:{}]", id))
            .unwrap_or_default();
        let sender = if msg.is_bot && msg.display_name == "Alma" {
            "[Alma (YOU)]".to_string()
        } else if msg.is_bot {
            format!("[{} (BOT)]", msg.display_name)
        } else {
            format!("[{}]", msg.display_name)
        };
        let text = truncate_with_ellipsis(&msg.text, 300);
        lines.push(format!("[{}]{} {}: {}", time, message_id, sender, text));
    }

    lines.push("---END HISTORY---".to_string());
    lines.join("\n")
}

fn append_to_alma_chat_log(
    chat_id: i64,
    display_name: &str,
    text: &str,
    is_alma: bool,
    timestamp_secs: u64,
    message_id: Option<i64>,
    user_id: Option<i64>,
    username: Option<&str>,
) -> Result<(), std::io::Error> {
    let Some(home) = dirs::home_dir() else {
        return Ok(());
    };
    let dir = home.join(".config/alma/chats");
    std::fs::create_dir_all(&dir)?;

    let instant = timestamp_or_now(timestamp_secs);
    let date = format!(
        "{:04}-{:02}-{:02}",
        instant.year(),
        u8::from(instant.month()),
        instant.day()
    );
    let clock = format!(
        "{:02}:{:02}:{:02}",
        instant.hour(),
        instant.minute(),
        instant.second()
    );
    let path = dir.join(format!("{}_{}.log", chat_id, date));
    let msg = message_id
        .map(|id| format!(" [msg:{}]", id))
        .unwrap_or_default();
    let sender = if is_alma {
        "[Alma]".to_string()
    } else if let Some(uid) = user_id {
        match username {
            Some(name) if !name.is_empty() => {
                format!("[{} (@{}) [id:{}]]", display_name, name, uid)
            }
            _ => format!("[{} [id:{}]]", display_name, uid),
        }
    } else {
        format!("[{}]", display_name)
    };
    let one_line = text.replace('\n', " ");
    let line = format!("[{}]{} {}: {}\n", clock, msg, sender, one_line);
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(line.as_bytes())
}

fn format_history_time(timestamp_secs: u64) -> String {
    let instant = timestamp_or_now(timestamp_secs)
        .to_offset(time::UtcOffset::from_hms(8, 0, 0).unwrap_or(time::UtcOffset::UTC));
    format!("{:02}:{:02}", instant.hour(), instant.minute())
}

fn timestamp_or_now(timestamp_secs: u64) -> time::OffsetDateTime {
    if timestamp_secs == 0 {
        return time::OffsetDateTime::now_utc();
    }
    time::OffsetDateTime::from_unix_timestamp(timestamp_secs as i64)
        .unwrap_or_else(|_| time::OffsetDateTime::now_utc())
}

fn current_unix_timestamp() -> u64 {
    time::OffsetDateTime::now_utc().unix_timestamp().max(0) as u64
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

        let mut boundary = floor_char_boundary(remaining, limit);
        if boundary == 0 {
            boundary = remaining
                .chars()
                .next()
                .map(|ch| ch.len_utf8())
                .unwrap_or(remaining.len());
        }
        let split_at = match remaining[..boundary].rfind('\n') {
            Some(idx) if idx > 0 => idx,
            _ => boundary,
        };

        chunks.push(remaining[..split_at].to_string());
        remaining = &remaining[split_at..];

        if remaining.starts_with('\n') {
            remaining = &remaining[1..];
        }
    }

    chunks
}

fn single_line_preview(text: &str, limit: usize) -> String {
    let compact = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");

    truncate_with_ellipsis(&compact, limit)
}

fn truncate_with_ellipsis(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_string();
    }

    let end = floor_char_boundary(text, limit);
    format!("{}...", &text[..end])
}

fn floor_char_boundary(text: &str, max_bytes: usize) -> usize {
    if text.len() <= max_bytes {
        return text.len();
    }

    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    end
}

fn cleaned_command_for_group(text: &str) -> bool {
    let trimmed = text.trim();
    if !trimmed.starts_with('/') {
        return false;
    }
    let cmd = trimmed.split_whitespace().next().unwrap_or("");
    let cmd = cmd.trim_start_matches('/');
    let cmd = cmd.split('@').next().unwrap_or("").to_ascii_lowercase();
    matches!(
        cmd.as_str(),
        "start" | "help" | "new" | "list" | "switch" | "status" | "settings"
    )
}

fn build_telegram_like_message_text(
    is_group: bool,
    _is_reply_to_bot: bool,
    display_name: &str,
    username: Option<&str>,
    user_id: i64,
    is_bot_sender: bool,
    message_id: i64,
    text: &str,
    forward_context: Option<&str>,
    reply_context: Option<&str>,
) -> String {
    let mut body = text.trim().to_string();
    if let Some(prefix) = forward_context {
        if !prefix.is_empty() {
            body = if body.is_empty() {
                prefix.to_string()
            } else {
                format!("{} {}", prefix, body)
            };
        }
    }
    if let Some(prefix) = reply_context {
        if !prefix.is_empty() {
            body = if body.is_empty() {
                prefix.to_string()
            } else {
                format!("{} {}", prefix, body)
            };
        }
    }

    if is_group {
        let username_part = username.map(|u| format!(" (@{})", u)).unwrap_or_default();
        let bot_suffix = if is_bot_sender { " [BOT]" } else { "" };
        format!(
            "[From: {}{} [id:{}] [msg:{}]{}] {}",
            display_name, username_part, user_id, message_id, bot_suffix, body
        )
    } else {
        format!("[msg:{}] {}", message_id, body)
    }
}

fn build_telegram_like_channel_system_context(
    is_group: bool,
    username: &str,
    group_title: Option<&str>,
    group_id: Option<i64>,
) -> String {
    let username = if username.trim().is_empty() {
        "unknown"
    } else {
        username.trim()
    };
    let platform = std::env::consts::OS;

    if is_group {
        let title = group_title.unwrap_or("Unknown");
        let chat_id = group_id.unwrap_or(0);
        format!(
            "[System: The user is chatting with you via Telegram (username: @{}) in a GROUP CHAT named \"{}\" (chatId: {}). But remember — you live on the user's {} computer, not inside Telegram. Telegram is just the communication channel. You have full access to the operating system: you can take screenshots, run commands, read/write files, browse the web, and do anything a desktop AI agent can do.]",
            username, title, chat_id, platform
        )
    } else {
        format!(
            "[System: The user is chatting with you via Telegram (username: @{}). But remember — you live on the user's {} computer, not inside Telegram. Telegram is just the communication channel. You have full access to the operating system: you can take screenshots, run commands, read/write files, browse the web, and do anything a desktop AI agent can do.]",
            username, platform
        )
    }
}

/// Download a media URL and encode it as a Base64 data URI in an Alma file part JSON object.
async fn download_media_as_file_part(
    client: &reqwest::Client,
    url: &str,
    default_filename: &str,
) -> Result<serde_json::Value, String> {
    use base64::prelude::*;

    let parsed_url = reqwest::Url::parse(url).map_err(|e| format!("Invalid media URL: {}", e))?;
    match parsed_url.scheme() {
        "http" | "https" => {}
        scheme => return Err(format!("Unsupported media URL scheme: {}", scheme)),
    }

    debug!("[Alma] Downloading media from URL: {}", url);
    let resp = client
        .get(parsed_url.clone())
        .send()
        .await
        .map_err(|e| format!("Failed to fetch URL: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("HTTP status error: {}", resp.status()));
    }

    if let Some(len) = resp.content_length() {
        if len > MAX_MEDIA_BYTES {
            return Err(format!(
                "Media too large: {} bytes exceeds {} byte limit",
                len, MAX_MEDIA_BYTES
            ));
        }
    }

    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            let path = parsed_url.path();
            if path.ends_with(".png") {
                "image/png".to_string()
            } else if path.ends_with(".jpg") || path.ends_with(".jpeg") {
                "image/jpeg".to_string()
            } else if path.ends_with(".gif") {
                "image/gif".to_string()
            } else {
                "application/octet-stream".to_string()
            }
        });

    let mut bytes = Vec::new();
    let mut resp = resp;
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| format!("Failed to read response bytes: {}", e))?
    {
        if bytes.len() as u64 + chunk.len() as u64 > MAX_MEDIA_BYTES {
            return Err(format!(
                "Media too large: response exceeds {} byte limit",
                MAX_MEDIA_BYTES
            ));
        }
        bytes.extend_from_slice(&chunk);
    }

    let b64_data = BASE64_STANDARD.encode(&bytes);

    let filename = {
        let mut name = None;
        if let Some(segments) = parsed_url.path_segments() {
            for seg in segments {
                if !seg.is_empty() {
                    name = Some(seg.to_string());
                }
            }
        }
        match name {
            Some(clean_seg) if clean_seg.contains('.') => clean_seg,
            _ => default_filename.to_string(),
        }
    };

    Ok(serde_json::json!({
        "type": "file",
        "mediaType": content_type,
        "url": format!("data:{};base64,{}", content_type, b64_data),
        "filename": filename
    }))
}

#[cfg(test)]
mod tests {
    use super::{
        build_recent_chat_history_context, build_telegram_like_channel_system_context,
        build_telegram_like_message_text, cleaned_command_for_group, download_media_as_file_part,
        resolve_thread_for_session, single_line_preview, split_text,
    };
    use crate::alma_ws::normalize_assistant_text;
    use crate::config::Config;
    use crate::state::SharedState;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::oneshot;

    fn temp_db_path(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("alma-onebot-bridge-{name}-{nonce}.db"))
    }

    async fn spawn_create_thread_server(
        thread_id: &'static str,
    ) -> (String, oneshot::Receiver<String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (req_tx, req_rx) = oneshot::channel();

        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = vec![0_u8; 4096];
            let n = stream.read(&mut buf).await.unwrap();
            let request = String::from_utf8_lossy(&buf[..n]).to_string();
            let _ = req_tx.send(request);

            let body = format!(r#"{{"id":"{}"}}"#, thread_id);
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        });

        (format!("http://{}", addr), req_rx)
    }

    #[test]
    fn single_line_preview_compacts_reply_context() {
        assert_eq!(
            single_line_preview("看看这个\n[Image]\n", 200),
            "看看这个 [Image]"
        );
    }

    #[test]
    fn single_line_preview_truncates_on_char_boundary() {
        let preview = single_line_preview("你好世界你好世界你好世界", 7);
        assert!(preview.ends_with("..."));
        assert!(!preview.contains('\u{fffd}'));
    }

    #[test]
    fn normalized_short_reply_is_stable_for_dedup() {
        let text = normalize_assistant_text("萌依收到电报");
        assert_eq!(text.trim(), "萌依收到电报");
    }

    #[test]
    fn group_reply_to_bot_uses_from_header_like_telegram() {
        let text = build_telegram_like_message_text(
            true,
            true,
            "群名片",
            Some("alice"),
            42,
            false,
            99,
            "你好",
            None,
            Some("[Replying to Alma's message: \"前文\"]"),
        );

        assert_eq!(
            text,
            "[From: 群名片 (@alice) [id:42] [msg:99]] [Replying to Alma's message: \"前文\"] 你好"
        );
    }

    #[test]
    fn normal_group_message_uses_from_header_like_telegram() {
        let text = build_telegram_like_message_text(
            true,
            false,
            "Alice",
            None,
            42,
            false,
            100,
            "你好",
            Some("[Forwarded message]"),
            None,
        );

        assert_eq!(
            text,
            "[From: Alice [id:42] [msg:100]] [Forwarded message] 你好"
        );
    }

    #[test]
    fn private_message_uses_msg_prefix_like_telegram() {
        let text = build_telegram_like_message_text(
            false, false, "Alice", None, 42, false, 100, "你好", None, None,
        );

        assert_eq!(text, "[msg:100] 你好");
    }

    #[test]
    fn recent_chat_history_marks_alma_and_message_ids() {
        let history = vec![
            crate::state::GroupMessage {
                display_name: "Alice".to_string(),
                text: "你好".to_string(),
                timestamp: 1_718_000_000,
                message_id: Some(100),
                is_bot: false,
            },
            crate::state::GroupMessage {
                display_name: "Alma".to_string(),
                text: "在".to_string(),
                timestamp: 1_718_000_060,
                message_id: Some(101),
                is_bot: true,
            },
        ];

        let ctx = build_recent_chat_history_context(&history);

        assert!(ctx.starts_with("RECENT CHAT HISTORY:"));
        assert!(ctx.contains("[msg:100] [Alice]: 你好"));
        assert!(ctx.contains("[msg:101] [Alma (YOU)]: 在"));
        assert!(ctx.ends_with("---END HISTORY---"));
    }

    #[test]
    fn split_text_keeps_cjk_char_boundaries() {
        let chunks = split_text("你好世界", 5);

        assert_eq!(chunks, vec!["你", "好", "世", "界"]);
    }

    #[tokio::test]
    async fn download_media_rejects_non_http_urls() {
        let client = reqwest::Client::new();
        let err = download_media_as_file_part(&client, "file:///tmp/a.png", "a.png")
            .await
            .unwrap_err();

        assert!(err.contains("Unsupported media URL scheme"));
    }

    #[tokio::test]
    async fn resolving_thread_does_not_touch_locked_alma_database() {
        let locked_alma_db_path = temp_db_path("locked-alma");
        let bridge_db_path = temp_db_path("bridge-state");

        let locked_db = turso::Builder::new_local(locked_alma_db_path.to_string_lossy().as_ref())
            .build()
            .await
            .unwrap();
        let lock_conn = locked_db.connect().unwrap();
        lock_conn
            .execute("CREATE TABLE lock_probe (id INTEGER PRIMARY KEY)", ())
            .await
            .unwrap();
        lock_conn.execute("BEGIN EXCLUSIVE", ()).await.unwrap();
        lock_conn
            .execute("INSERT INTO lock_probe (id) VALUES (1)", ())
            .await
            .unwrap();

        let (alma_api, req_rx) = spawn_create_thread_server("thread-from-rest").await;
        let mut config = Config::from_env();
        config.alma_api = alma_api;
        config.db_path = bridge_db_path.clone();
        let state = SharedState::new(config).await.unwrap();

        let (thread_id, existed) =
            resolve_thread_for_session(&state, "group:706968284", true, 706968284, "萌依")
                .await
                .unwrap();

        assert_eq!(thread_id, "thread-from-rest");
        assert!(!existed);
        assert_eq!(
            state.get_thread_id("group:706968284").await.as_deref(),
            Some("thread-from-rest")
        );
        assert!(
            req_rx.await.unwrap().starts_with("POST /api/threads "),
            "thread resolution should create threads through Alma REST only"
        );

        lock_conn.execute("ROLLBACK", ()).await.unwrap();
        drop(lock_conn);
        drop(locked_db);
        let _ = std::fs::remove_file(locked_alma_db_path);
        let _ = std::fs::remove_file(bridge_db_path);
    }

    #[test]
    fn recognized_group_commands_match_telegram_trigger_set() {
        assert!(cleaned_command_for_group("/start"));
        assert!(cleaned_command_for_group("/help@alma_bot hi"));
        assert!(cleaned_command_for_group("/settings"));
        assert!(!cleaned_command_for_group("/unknown"));
        assert!(!cleaned_command_for_group("hello"));
    }

    #[test]
    fn group_system_context_matches_telegram_style_shape() {
        let ctx =
            build_telegram_like_channel_system_context(true, "alice", Some("测试群"), Some(123));

        assert!(ctx.starts_with("[System: The user is chatting with you via Telegram"));
        assert!(ctx.contains("username: @alice"));
        assert!(ctx.contains("in a GROUP CHAT named \"测试群\" (chatId: 123)"));
        assert!(ctx.ends_with("desktop AI agent can do.]"));
    }
}
