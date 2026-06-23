use futures_util::future::join_all;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use warp::ws::Message;

use crate::alma;
use crate::alma_ws::{AlmaEvent, AlmaWsClient};
use crate::group_log;
use crate::onebot::event::{
    OneBotEvent, contains_at_bot, convert_faces_to_text, extract_forward_id, extract_images,
    extract_media_summary, extract_reply_id, extract_text, has_media_segments,
};
use crate::onebot::{
    PendingCalls, get_forward_msg, get_group_name, get_msg, send_reply_message, send_text_message,
};
use crate::people::ensure_people_profile;
use crate::state::SharedState;

pub(crate) const QQ_MSG_LIMIT: usize = 4500;
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

    // ── Snapshot config values (avoids repeated RwLock reads) ──────────────
    let (onebot_timeout, thinking_msg_cfg, show_thinking, max_retries, base_delay_ms, run_timeout) = {
        let cfg = state.config.read().await;
        (
            cfg.onebot_api_timeout_secs,
            cfg.thinking_message.clone(),
            cfg.show_thinking,
            cfg.alma_max_retries,
            cfg.alma_retry_delay_ms,
            cfg.alma_run_timeout_secs,
        )
    };

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
    let event_time = event.time.unwrap_or_else(current_unix_timestamp);
    let user_id = event
        .user_id
        .ok_or_else(|| "OneBot message event missing user_id".to_string())?;
    let group_id = if is_group {
        Some(
            event
                .group_id
                .filter(|id| *id > 0)
                .ok_or_else(|| "OneBot group message event missing group_id".to_string())?,
        )
    } else {
        None
    };
    let group_id_value = group_id.unwrap_or_default();
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

    let group_title = resolve_group_title(
        state,
        ws_tx,
        pending,
        is_group,
        group_id_value,
        onebot_timeout,
    )
    .await;

    // ── Record to group history (before @bot gate, so ALL messages are captured) ──
    let observed_text = observed_message_text(&text, &media_lines);
    if is_group && !observed_text.is_empty() {
        record_observed_group_message(
            state,
            ObservedGroupMessage {
                group_id: group_id_value,
                group_title: group_title.as_deref(),
                user_id,
                display_name,
                text: &observed_text,
                timestamp_secs: event_time,
                message_id: event.message_id,
            },
        )
        .await;
    }

    // ── Message ID & Reply context ────────────────────────────────────────
    // Use real OneBot message_id for [msg:N] (matches Telegram bridge pattern)
    let message_id = event
        .message_id
        .ok_or_else(|| "OneBot message event missing message_id".to_string())?;

    // Handle reply/quoting: extract reply segment and fetch quoted message
    let mut quoted_image_urls = Vec::new();
    let mut is_reply_to_bot = false;
    let reply_context = if let Some(reply_msg_id) = extract_reply_id(segments) {
        match get_msg(ws_tx, pending, &reply_msg_id, onebot_timeout).await {
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
        format!("群{}", group_id_value)
    } else {
        "私聊".to_string()
    };
    info!("[Message] {} {}: {}", source, display_name, cleaned_text);

    // ── Forwarded message content ─────────────────────────────────────────
    // If the message contains a forward segment, fetch the forwarded content
    let forward_context = if let Some(forward_id) = extract_forward_id(segments) {
        match get_forward_msg(ws_tx, pending, &forward_id, onebot_timeout).await {
            Ok(nodes) if !nodes.is_empty() => {
                let summaries: Vec<String> = nodes
                    .iter()
                    .take(20) // Limit to first 20 nodes to avoid huge messages
                    .map(|(name, text)| {
                        let truncated = truncate_with_ellipsis(text, 100);
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
    let group_id_opt = group_id;
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
        format!("group:{}", group_id_value)
    } else {
        format!("private:{}", user_id)
    };

    let (thread_id, has_existing_thread) = resolve_thread_for_session(
        state,
        &session_key,
        is_group,
        group_id_value,
        sender_nickname,
    )
    .await?;

    // ── Call Alma AI via WebSocket (full chat pipeline) ──────────────────
    if let Err(e) = ensure_alma_ws_client(state).await {
        warn!("[Alma] WebSocket client not connected: {}", e);
        let target_id = if is_group { group_id_value } else { user_id };
        let target_type = if is_group { "group" } else { "private" };
        let _ = send_text_message(
            ws_tx,
            pending,
            target_type,
            target_id,
            ALMA_ERROR_REPLY,
            onebot_timeout,
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
    let file_parts = build_file_parts(state, &image_urls, &quoted_image_urls, segments).await;

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
    let ephemeral_ctx = build_ephemeral_context(
        state,
        EphemeralContextInput {
            is_group,
            sender_nickname,
            group_title: group_title.as_deref(),
            group_id,
            group_id_value,
            user_id,
            display_name,
        },
    )
    .await;

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
    if let Some(ref thinking_msg) = thinking_msg_cfg {
        let target_id = if is_group { group_id_value } else { user_id };
        let target_type = if is_group { "group" } else { "private" };
        match send_text_message(
            ws_tx,
            pending,
            target_type,
            target_id,
            thinking_msg,
            onebot_timeout,
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
        let mut last_err = String::new();

        let mut result = None;
        let mut thinking_content = None;
        for attempt in 0..=max_retries {
            if attempt > 0 {
                let delay = retry_delay_ms(base_delay_ms, attempt); // exponential backoff
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
                    run_timeout,
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

    let target_id = if is_group { group_id_value } else { user_id };
    let target_type = if is_group { "group" } else { "private" };

    // ── Send thinking content as separate message (if enabled) ────────────
    if show_thinking
        && let Some(ref think_text) = thinking
        && !think_text.is_empty()
    {
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
                onebot_timeout,
            )
            .await
            {
                Ok(_) => {}
                Err(e) => tracing::debug!("[Thinking] Failed to send thinking: {}", e),
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
                        onebot_timeout,
                    )
                    .await
                } else {
                    send_text_message(
                        ws_tx,
                        pending,
                        target_type,
                        target_id,
                        chunk,
                        onebot_timeout,
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
                    onebot_timeout,
                )
                .await
            };

            match result {
                Ok(resp) => {
                    state.register_sent_reply(&thread_id, chunk).await;
                    let msg_id = resp
                        .data
                        .as_ref()
                        .and_then(|d| d.get("message_id"))
                        .and_then(|m| m.as_i64());
                    if is_group {
                        record_alma_group_output(
                            state,
                            group_id_value,
                            chunk,
                            msg_id,
                            current_unix_timestamp(),
                        )
                        .await;
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

async fn resolve_group_title(
    state: &SharedState,
    ws_tx: &mpsc::UnboundedSender<Message>,
    pending: &PendingCalls,
    is_group: bool,
    group_id: i64,
    timeout_secs: u64,
) -> Option<String> {
    if !is_group {
        return None;
    }

    if let Some(title) = state.get_group_title(group_id).await {
        return Some(title);
    }

    match get_group_name(ws_tx, pending, group_id, timeout_secs).await {
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

struct ObservedGroupMessage<'a> {
    group_id: i64,
    group_title: Option<&'a str>,
    user_id: i64,
    display_name: &'a str,
    text: &'a str,
    timestamp_secs: u64,
    message_id: Option<i64>,
}

async fn record_observed_group_message(state: &SharedState, message: ObservedGroupMessage<'_>) {
    let session_key = format!("group:{}", message.group_id);
    if let Err(e) = state
        .touch_group(
            message.group_id,
            message.group_title,
            message.timestamp_secs,
        )
        .await
    {
        debug!(
            "[GroupDirectory] Failed to touch group {}: {}",
            message.group_id, e
        );
    }
    if message.user_id != 0
        && let Err(e) = state
            .record_group_member(
                message.group_id,
                message.user_id,
                message.display_name,
                message.timestamp_secs,
            )
            .await
    {
        debug!(
            "[GroupDirectory] Failed to record member {} in group {}: {}",
            message.user_id, message.group_id, e
        );
    }

    state
        .record_group_message(
            &session_key,
            crate::state::GroupMessage {
                display_name: message.display_name.to_string(),
                text: message.text.to_string(),
                timestamp: message.timestamp_secs,
                message_id: message.message_id,
                is_bot: false,
            },
        )
        .await;

    if let Err(e) = group_log::append_alma_group_log_async(
        message.group_id,
        message.display_name.to_string(),
        message.text.to_string(),
        false,
        message.timestamp_secs,
        message.message_id,
        Some(message.user_id),
        None,
    )
    .await
    {
        debug!("[GroupHistory] Failed to append Alma group log: {}", e);
    }

    refresh_alma_group_directory_readme(state).await;
    debug!(
        "[GroupHistory] Recorded message from {} in {}",
        message.display_name, session_key
    );
}

async fn build_file_parts(
    state: &SharedState,
    image_urls: &[String],
    quoted_image_urls: &[String],
    segments: &[crate::onebot::event::MessageSegment],
) -> Vec<serde_json::Value> {
    let mut downloads = Vec::new();

    for (idx, url) in image_urls.iter().enumerate() {
        let default_filename = format!("image_{}.png", idx + 1);
        downloads.push(("image", default_filename, url.clone()));
    }

    // Download quoted/replied images too, so Alma can see the referenced image
    // instead of only a textual placeholder like "[Image]".
    for (idx, url) in quoted_image_urls.iter().enumerate() {
        let default_filename = format!("quoted_image_{}.png", idx + 1);
        downloads.push(("quoted image", default_filename, url.clone()));
    }

    for (filename, url) in &crate::onebot::event::extract_files(segments) {
        downloads.push(("file", filename.clone(), url.clone()));
    }

    let client = state.http_client.clone();
    let results = join_all(downloads.into_iter().map(|(kind, filename, url)| {
        let client = client.clone();
        async move {
            let result = download_media_as_file_part(&client, &url, &filename).await;
            (kind, filename, url, result)
        }
    }))
    .await;

    let mut file_parts = Vec::new();
    for (kind, filename, url, result) in results {
        match result {
            Ok(part) => {
                info!(
                    "[Alma] Successfully downloaded and prepared {} part: {} ({})",
                    kind, filename, url
                );
                file_parts.push(part);
            }
            Err(e) => warn!("[Alma] Failed to download {} {}: {}", kind, url, e),
        }
    }

    file_parts
}

struct EphemeralContextInput<'a> {
    is_group: bool,
    sender_nickname: &'a str,
    group_title: Option<&'a str>,
    group_id: Option<i64>,
    group_id_value: i64,
    user_id: i64,
    display_name: &'a str,
}

async fn build_ephemeral_context(state: &SharedState, input: EphemeralContextInput<'_>) -> String {
    let mut ephemeral_ctx = build_telegram_like_channel_system_context(
        input.is_group,
        input.sender_nickname,
        input.group_title,
        input.group_id,
        state.config.read().await.bridge_port,
    );

    if let Some(profile_block) =
        crate::people::find_sender_profile(state, &input.user_id.to_string(), input.display_name)
            .await
    {
        ephemeral_ctx.push_str(&profile_block);
    }

    let profile_count = crate::people::count_profiles(state).await;
    if profile_count > 0 {
        ephemeral_ctx.push_str(&format!(
            "\n\nPEOPLE PROFILES — You know {} people. Use `alma people list` or `alma people show <name>` to look up profiles on demand.",
            profile_count
        ));
    }

    if input.is_group {
        let history_session_key = format!("group:{}", input.group_id_value);
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

    ephemeral_ctx
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

    // ── Snapshot config values ──────────────────────────────────────────────
    let (show_thinking, onebot_timeout) = {
        let cfg = state.config.read().await;
        (cfg.show_thinking, cfg.onebot_api_timeout_secs)
    };

    // Only forward assistant messages from threads we're tracking
    let target = match state.get_qq_target(&event.thread_id).await? {
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

    if show_thinking
        && let Some(ref think_text) = event.thinking_text
        && !think_text.is_empty()
    {
        let think_chunks = split_text(think_text, QQ_MSG_LIMIT);
        for chunk in &think_chunks {
            match send_text_message(
                ws_tx,
                pending,
                &target.target_type,
                target.target_id,
                chunk,
                onebot_timeout,
            )
            .await
            {
                Ok(_) => {}
                Err(e) => tracing::debug!("[Alma→QQ] Failed to forward thinking: {}", e),
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
            onebot_timeout,
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
                    record_alma_group_output(
                        state,
                        target.target_id,
                        chunk,
                        msg_id,
                        current_unix_timestamp(),
                    )
                    .await;
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
    if let Some(thread_id) = state.get_thread_id(session_key).await? {
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
        .await?;
    Ok((thread_id, false))
}

async fn ensure_alma_ws_client(state: &SharedState) -> Result<AlmaWsClient, String> {
    if let Some(client) = state.get_alma_ws().await {
        if !client.is_connected() {
            warn!("[Alma] Existing WebSocket is reconnecting");
        }
        return Ok(client);
    }

    let client = AlmaWsClient::connect(&state.config.read().await.alma_api).await?;
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

pub(crate) async fn record_alma_group_output(
    state: &SharedState,
    group_id: i64,
    text: &str,
    message_id: Option<i64>,
    timestamp_secs: u64,
) {
    let history_session_key = format!("group:{}", group_id);
    state
        .record_group_message(
            &history_session_key,
            crate::state::GroupMessage {
                display_name: "Alma".to_string(),
                text: text.to_string(),
                timestamp: timestamp_secs,
                message_id,
                is_bot: true,
            },
        )
        .await;
    if let Err(e) = state.touch_group(group_id, None, timestamp_secs).await {
        debug!("[GroupDirectory] Failed to touch group {}: {}", group_id, e);
    }
    if let Err(e) = group_log::append_alma_group_log_async(
        group_id,
        "Alma".to_string(),
        text.to_string(),
        true,
        timestamp_secs,
        message_id,
        None,
        None,
    )
    .await
    {
        debug!("[GroupHistory] Failed to append Alma group log: {}", e);
    }
    refresh_alma_group_directory_readme(state).await;
}

pub(crate) async fn refresh_alma_group_directory_readme(state: &SharedState) {
    match state.group_directory_snapshot().await {
        Ok(entries) => {
            let bridge_port = state.config.read().await.bridge_port;
            if let Err(e) = group_log::write_group_readme_async(entries, bridge_port).await {
                debug!("[GroupDirectory] Failed to write README: {}", e);
            }
        }
        Err(e) => debug!("[GroupDirectory] Failed to snapshot groups: {}", e),
    }
}

fn observed_message_text(text: &str, media_lines: &[String]) -> String {
    let text = text.trim();
    let media = media_lines
        .iter()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");

    match (text.is_empty(), media.is_empty()) {
        (true, true) => String::new(),
        (false, true) => text.to_string(),
        (true, false) => media,
        (false, false) => format!("{} {}", text, media),
    }
}

fn format_history_time(timestamp_secs: u64) -> String {
    let instant = timestamp_or_now(timestamp_secs).to_offset(local_offset());
    format!("{:02}:{:02}", instant.hour(), instant.minute())
}

fn local_offset() -> time::UtcOffset {
    time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC)
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
///
/// Limits smaller than one UTF-8 scalar value are rounded up so splitting never
/// produces invalid text. The production QQ limit is much larger than this.
pub(crate) fn split_text(text: &str, limit: usize) -> Vec<String> {
    let limit = limit.max(4);
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

fn retry_delay_ms(base_delay_ms: u64, attempt: u32) -> u64 {
    const MAX_RETRY_DELAY_MS: u64 = 60_000;

    let exponent = attempt.saturating_sub(1).min(20);
    let multiplier = 1_u64.checked_shl(exponent).unwrap_or(u64::MAX);
    base_delay_ms
        .saturating_mul(multiplier)
        .min(MAX_RETRY_DELAY_MS)
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

#[allow(clippy::too_many_arguments)]
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
    if let Some(prefix) = forward_context
        && !prefix.is_empty()
    {
        body = if body.is_empty() {
            prefix.to_string()
        } else {
            format!("{} {}", prefix, body)
        };
    }
    if let Some(prefix) = reply_context
        && !prefix.is_empty()
    {
        body = if body.is_empty() {
            prefix.to_string()
        } else {
            format!("{} {}", prefix, body)
        };
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
    bridge_port: u16,
) -> String {
    let username = if username.trim().is_empty() {
        "unknown"
    } else {
        username.trim()
    };
    let platform = std::env::consts::OS;

    if is_group {
        let title = group_title.unwrap_or("Unknown");
        if let Some(chat_id) = group_id {
            format!(
                "[System: The user is chatting with you via Telegram (username: @{}) in a GROUP CHAT named \"{}\" (chatId: {}). But remember — you live on the user's {} computer, not inside Telegram. Telegram is just the communication channel. You have full access to the operating system: you can take screenshots, run commands, read/write files, browse the web, and do anything a desktop AI agent can do.]\n\n[QQ BRIDGE: This Telegram-compatible channel is actually QQ / OneBot. For QQ group reading, `alma group list`, `alma group history {} 100`, `alma group search <keyword>`, and `cat ~/.config/alma/groups/README.md` work because alma-onebot-bridge writes QQ logs in Alma's native group-log format. `alma group context {}` can read local logs, but Telegram-only API details may be unavailable for QQ. For active QQ group sending, do not use `alma group send`; that command targets Telegram. Use `curl -s -X POST http://127.0.0.1:{}/qq/group/{}/send -H 'Content-Type: application/json' -d '{{\"message\":\"...\"}}'`.]",
                username, title, chat_id, platform, chat_id, chat_id, bridge_port, chat_id
            )
        } else {
            format!(
                "[System: The user is chatting with you via Telegram (username: @{}) in a GROUP CHAT named \"{}\". But remember — you live on the user's {} computer, not inside Telegram. Telegram is just the communication channel. You have full access to the operating system: you can take screenshots, run commands, read/write files, browse the web, and do anything a desktop AI agent can do.]\n\n[QQ BRIDGE: This Telegram-compatible channel is actually QQ / OneBot, but this message did not include a valid QQ group id. Do not invent chatId 0 or use group-send commands for this turn.]",
                username, title, platform
            )
        }
    } else {
        format!(
            "[System: The user is chatting with you via Telegram (username: @{}). But remember — you live on the user's {} computer, not inside Telegram. Telegram is just the communication channel. You have full access to the operating system: you can take screenshots, run commands, read/write files, browse the web, and do anything a desktop AI agent can do.]\n\n[QQ BRIDGE: This Telegram-compatible private chat is actually QQ / OneBot. For active QQ private sending, use `curl -s -X POST http://127.0.0.1:{}/qq/private/<userId>/send -H 'Content-Type: application/json' -d '{{\"message\":\"...\"}}'`. Do not use Telegram-specific send commands for QQ.]",
            username, platform, bridge_port
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
    let resp = tokio::time::timeout(
        tokio::time::Duration::from_secs(30),
        client.get(parsed_url.clone()).send(),
    )
    .await
    .map_err(|_| "Timed out fetching media URL".to_string())?
    .map_err(|e| format!("Failed to fetch URL: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("HTTP status error: {}", resp.status()));
    }

    if let Some(len) = resp.content_length()
        && len > MAX_MEDIA_BYTES
    {
        return Err(format!(
            "Media too large: {} bytes exceeds {} byte limit",
            len, MAX_MEDIA_BYTES
        ));
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
    while let Some(chunk) = tokio::time::timeout(tokio::time::Duration::from_secs(30), resp.chunk())
        .await
        .map_err(|_| "Timed out reading media response".to_string())?
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
        resolve_thread_for_session, retry_delay_ms, single_line_preview, split_text,
        truncate_with_ellipsis,
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
    fn truncate_with_ellipsis_keeps_cjk_char_boundaries() {
        let preview = truncate_with_ellipsis("你好世界你好世界你好世界", 100);
        assert_eq!(preview, "你好世界你好世界你好世界");

        let preview = truncate_with_ellipsis("你好世界你好世界你好世界", 7);
        assert_eq!(preview, "你好...");
    }

    #[test]
    fn retry_delay_saturates_instead_of_overflowing() {
        assert_eq!(retry_delay_ms(3_000, 1), 3_000);
        assert_eq!(retry_delay_ms(3_000, 2), 6_000);
        assert_eq!(retry_delay_ms(u64::MAX, 64), 60_000);
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

    #[test]
    fn split_text_tiny_limit_still_makes_utf8_progress() {
        let chunks = split_text("你好", 1);

        assert_eq!(chunks, vec!["你", "好"]);
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
        let mut config = Config::load();
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
            state
                .get_thread_id("group:706968284")
                .await
                .unwrap()
                .as_deref(),
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
        let ctx = build_telegram_like_channel_system_context(
            true,
            "alice",
            Some("测试群"),
            Some(123),
            8090,
        );

        assert!(ctx.starts_with("[System: The user is chatting with you via Telegram"));
        assert!(ctx.contains("username: @alice"));
        assert!(ctx.contains("in a GROUP CHAT named \"测试群\" (chatId: 123)"));
        assert!(ctx.contains("alma group history 123 100"));
        assert!(ctx.contains("http://127.0.0.1:8090/qq/group/123/send"));
        assert!(ctx.contains("do not use `alma group send`"));
    }

    #[test]
    fn group_system_context_does_not_invent_zero_chat_id() {
        let ctx =
            build_telegram_like_channel_system_context(true, "alice", Some("测试群"), None, 8090);

        assert!(!ctx.contains("chatId: 0"));
        assert!(ctx.contains("did not include a valid QQ group id"));
    }
}
