use std::collections::HashMap;
use std::net::SocketAddr;

use serde::{Deserialize, Serialize};
use trillium::{Conn, Status};

use crate::auth::is_http_command_authorized;
use crate::onebot::{send_reply_message, send_text_message};
use crate::pipeline::{QQ_MSG_LIMIT, record_alma_group_output, split_text};
use crate::state::{GroupDirectoryEntry, SharedState};

pub struct JsonResponse {
    status: Status,
    body: String,
}

impl JsonResponse {
    pub fn new(status: Status, value: serde_json::Value) -> Self {
        Self {
            status,
            body: value.to_string(),
        }
    }

    pub fn into_conn(self, conn: Conn) -> Conn {
        conn.with_status(self.status)
            .with_response_header("content-type", "application/json; charset=utf-8")
            .with_body(self.body)
            .halt()
    }
}

pub fn health_response() -> JsonResponse {
    json_status(
        &serde_json::json!({
            "status": "ok",
            "service": "alma-onebot-bridge"
        }),
        Status::Ok,
    )
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
) -> JsonResponse {
    let access_token = state.config.read().await.access_token.clone();
    if !is_http_command_authorized(
        access_token.as_deref(),
        auth_header.as_deref(),
        &query,
        remote,
    ) {
        return json_status(
            &ErrorResponse {
                status: "error",
                error: "unauthorized".to_string(),
            },
            Status::Unauthorized,
        );
    }

    let groups = match state.group_directory_snapshot().await {
        Ok(groups) => groups,
        Err(e) => {
            return json_status(
                &ErrorResponse {
                    status: "error",
                    error: e,
                },
                Status::InternalServerError,
            );
        }
    };
    json_status(
        &GroupsResponse {
            status: "ok",
            onebot_connected: state.has_onebot_api_handle().await,
            readme_path: crate::group_log::alma_groups_dir()
                .map(|dir| dir.join("README.md").to_string_lossy().to_string()),
            groups,
        },
        Status::Ok,
    )
}

pub async fn send_group_message_handler(
    group_id: i64,
    body: SendMessageRequest,
    state: SharedState,
    auth_header: Option<String>,
    query: HashMap<String, String>,
    remote: Option<SocketAddr>,
) -> JsonResponse {
    send_message_handler("group", group_id, body, state, auth_header, query, remote).await
}

pub async fn send_private_message_handler(
    user_id: i64,
    body: SendMessageRequest,
    state: SharedState,
    auth_header: Option<String>,
    query: HashMap<String, String>,
    remote: Option<SocketAddr>,
) -> JsonResponse {
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
) -> JsonResponse {
    let (access_token, onebot_timeout) = {
        let cfg = state.config.read().await;
        (cfg.access_token.clone(), cfg.onebot_api_timeout_secs)
    };
    if !is_http_command_authorized(
        access_token.as_deref(),
        auth_header.as_deref(),
        &query,
        remote,
    ) {
        return json_status(
            &ErrorResponse {
                status: "error",
                error: "unauthorized".to_string(),
            },
            Status::Unauthorized,
        );
    }

    let message = body.message.trim();
    if message.is_empty() {
        return json_status(
            &ErrorResponse {
                status: "error",
                error: "message must not be empty".to_string(),
            },
            Status::BadRequest,
        );
    }

    let Some(handle) = state.get_onebot_api_handle().await else {
        return json_status(
            &ErrorResponse {
                status: "error",
                error: "no active OneBot reverse WebSocket connection".to_string(),
            },
            Status::ServiceUnavailable,
        );
    };

    let chunks = split_text(message, QQ_MSG_LIMIT);
    let session_key = format!("{}:{}", target_type, target_id);
    let thread_id = match state.get_thread_id(&session_key).await {
        Ok(thread_id) => thread_id,
        Err(e) => {
            return json_status(
                &ErrorResponse {
                    status: "error",
                    error: format!("failed to resolve bridge thread mapping: {}", e),
                },
                Status::InternalServerError,
            );
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
                    onebot_timeout,
                )
                .await
            } else {
                send_text_message(
                    &handle.ws_tx,
                    &handle.pending,
                    target_type,
                    target_id,
                    chunk,
                    onebot_timeout,
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
                onebot_timeout,
            )
            .await
        };

        let resp = match result {
            Ok(resp) => resp,
            Err(e) => {
                return json_status(
                    &ErrorResponse {
                        status: "error",
                        error: e,
                    },
                    Status::BadGateway,
                );
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

        if target_type == "group" {
            record_alma_group_output(&state, target_id, chunk, msg_id, current_unix_timestamp())
                .await;
        }
    }
    if let Some(thread_id) = thread_id.as_deref() {
        state.register_sent_reply(thread_id, message).await;
    }

    json_status(
        &SendMessageResponse {
            status: "ok",
            target_type,
            target_id,
            message_ids,
        },
        Status::Ok,
    )
}

fn json_status<T: Serialize>(value: &T, status: Status) -> JsonResponse {
    let (status, body) = match serde_json::to_string(value) {
        Ok(body) => (status, body),
        Err(e) => (
            Status::InternalServerError,
            serde_json::json!({
                "status": "error",
                "error": format!("failed to serialize response: {}", e),
            })
            .to_string(),
        ),
    };
    JsonResponse { status, body }
}

fn current_unix_timestamp() -> u64 {
    time::OffsetDateTime::now_utc().unix_timestamp().max(0) as u64
}
