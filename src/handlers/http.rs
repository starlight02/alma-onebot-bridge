use std::collections::HashMap;
use std::net::SocketAddr;

use serde::{Deserialize, Serialize};
use warp::http::StatusCode;

use crate::auth::is_http_command_authorized;
use crate::onebot::{send_reply_message, send_text_message};
use crate::pipeline::{QQ_MSG_LIMIT, record_alma_group_output, split_text};
use crate::state::{GroupDirectoryEntry, SharedState};

pub async fn health_handler() -> Result<impl warp::Reply, warp::Rejection> {
    Ok(warp::reply::json(&serde_json::json!({
        "status": "ok",
        "service": "alma-onebot-bridge"
    })))
}

#[derive(Debug, Deserialize)]
pub struct SendMessageRequest {
    pub message: String,
    pub reply_to_id: Option<String>,
    pub at_user_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct SendMessageResponse {
    status: &'static str,
    target_type: &'static str,
    target_id: i64,
    message_ids: Vec<i64>,
}

#[derive(Debug, Serialize)]
struct GroupsResponse {
    status: &'static str,
    onebot_connected: bool,
    readme_path: Option<String>,
    groups: Vec<GroupDirectoryEntry>,
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    status: &'static str,
    error: String,
}

pub async fn list_groups_handler(
    state: SharedState,
    auth_header: Option<String>,
    query: HashMap<String, String>,
    remote: Option<SocketAddr>,
) -> Result<impl warp::Reply, warp::Rejection> {
    if !is_http_command_authorized(
        state.config.access_token.as_deref(),
        auth_header.as_deref(),
        &query,
        remote,
    ) {
        return Ok(json_status(
            &ErrorResponse {
                status: "error",
                error: "unauthorized".to_string(),
            },
            StatusCode::UNAUTHORIZED,
        ));
    }

    let groups = match state.group_directory_snapshot().await {
        Ok(groups) => groups,
        Err(e) => {
            return Ok(json_status(
                &ErrorResponse {
                    status: "error",
                    error: e,
                },
                StatusCode::INTERNAL_SERVER_ERROR,
            ));
        }
    };
    Ok(json_status(
        &GroupsResponse {
            status: "ok",
            onebot_connected: state.has_onebot_api_handle().await,
            readme_path: crate::group_log::alma_groups_dir()
                .map(|dir| dir.join("README.md").to_string_lossy().to_string()),
            groups,
        },
        StatusCode::OK,
    ))
}

pub async fn send_group_message_handler(
    group_id: i64,
    body: SendMessageRequest,
    state: SharedState,
    auth_header: Option<String>,
    query: HashMap<String, String>,
    remote: Option<SocketAddr>,
) -> Result<impl warp::Reply, warp::Rejection> {
    send_message_handler("group", group_id, body, state, auth_header, query, remote).await
}

pub async fn send_private_message_handler(
    user_id: i64,
    body: SendMessageRequest,
    state: SharedState,
    auth_header: Option<String>,
    query: HashMap<String, String>,
    remote: Option<SocketAddr>,
) -> Result<impl warp::Reply, warp::Rejection> {
    send_message_handler("private", user_id, body, state, auth_header, query, remote).await
}

async fn send_message_handler(
    target_type: &'static str,
    target_id: i64,
    body: SendMessageRequest,
    state: SharedState,
    auth_header: Option<String>,
    query: HashMap<String, String>,
    remote: Option<SocketAddr>,
) -> Result<warp::reply::WithStatus<warp::reply::Json>, warp::Rejection> {
    if !is_http_command_authorized(
        state.config.access_token.as_deref(),
        auth_header.as_deref(),
        &query,
        remote,
    ) {
        return Ok(json_status(
            &ErrorResponse {
                status: "error",
                error: "unauthorized".to_string(),
            },
            StatusCode::UNAUTHORIZED,
        ));
    }

    let message = body.message.trim();
    if message.is_empty() {
        return Ok(json_status(
            &ErrorResponse {
                status: "error",
                error: "message must not be empty".to_string(),
            },
            StatusCode::BAD_REQUEST,
        ));
    }

    let Some(handle) = state.get_onebot_api_handle().await else {
        return Ok(json_status(
            &ErrorResponse {
                status: "error",
                error: "no active OneBot reverse WebSocket connection".to_string(),
            },
            StatusCode::SERVICE_UNAVAILABLE,
        ));
    };

    let chunks = split_text(message, QQ_MSG_LIMIT);
    let session_key = format!("{}:{}", target_type, target_id);
    let thread_id = match state.get_thread_id(&session_key).await {
        Ok(thread_id) => thread_id,
        Err(e) => {
            return Ok(json_status(
                &ErrorResponse {
                    status: "error",
                    error: format!("failed to resolve bridge thread mapping: {}", e),
                },
                StatusCode::INTERNAL_SERVER_ERROR,
            ));
        }
    };
    let mut message_ids = Vec::new();

    for (idx, chunk) in chunks.iter().enumerate() {
        let result = if idx == 0 {
            if let Some(reply_to_id) = body.reply_to_id.as_deref() {
                send_reply_message(
                    &handle.ws_tx,
                    &handle.pending,
                    target_type,
                    target_id,
                    chunk,
                    reply_to_id,
                    body.at_user_id.as_deref(),
                    state.config.onebot_api_timeout_secs,
                )
                .await
            } else {
                send_text_message(
                    &handle.ws_tx,
                    &handle.pending,
                    target_type,
                    target_id,
                    chunk,
                    state.config.onebot_api_timeout_secs,
                )
                .await
            }
        } else {
            send_text_message(
                &handle.ws_tx,
                &handle.pending,
                target_type,
                target_id,
                chunk,
                state.config.onebot_api_timeout_secs,
            )
            .await
        };

        let resp = match result {
            Ok(resp) => resp,
            Err(e) => {
                return Ok(json_status(
                    &ErrorResponse {
                        status: "error",
                        error: e,
                    },
                    StatusCode::BAD_GATEWAY,
                ));
            }
        };
        let msg_id = resp
            .data
            .as_ref()
            .and_then(|data| data.get("message_id"))
            .and_then(|message_id| message_id.as_i64());
        if let Some(msg_id) = msg_id {
            message_ids.push(msg_id);
        }

        if let Some(thread_id) = thread_id.as_deref() {
            state.register_sent_reply(thread_id, chunk).await;
        }

        if target_type == "group" {
            record_alma_group_output(&state, target_id, chunk, msg_id, current_unix_timestamp())
                .await;
        }
    }

    Ok(json_status(
        &SendMessageResponse {
            status: "ok",
            target_type,
            target_id,
            message_ids,
        },
        StatusCode::OK,
    ))
}

fn json_status<T: Serialize>(
    value: &T,
    status: StatusCode,
) -> warp::reply::WithStatus<warp::reply::Json> {
    warp::reply::with_status(warp::reply::json(value), status)
}

fn current_unix_timestamp() -> u64 {
    time::OffsetDateTime::now_utc().unix_timestamp().max(0) as u64
}
