use serde_json::Value;
use serde_json::json;
use std::time::Duration;
use tracing::{debug, info};

use crate::state::SharedState;

/// Per-request timeout for Alma control-plane REST calls.
///
/// These endpoints (create_thread, fetch_thread_model, fetch_default_model,
/// thread_exists) are normally fast, but Alma can stall under load. We give
/// each call a generous ceiling that comfortably exceeds the configured
/// generation timeout (`alma_run_timeout_secs`, default 120s) so a REST
/// failure is never blamed on a timeout the user can't see.
const ALMA_REST_TIMEOUT: Duration = Duration::from_secs(180);

async fn get_json(state: &SharedState, url: &str) -> Result<(u16, Value), String> {
    let conn = state
        .http_client
        .get(url)
        .with_timeout(ALMA_REST_TIMEOUT)
        .await
        .map_err(format_alma_http_error)?;
    read_json_response(conn).await
}

async fn post_json(state: &SharedState, url: &str, body: Value) -> Result<(u16, Value), String> {
    let conn = state
        .http_client
        .post(url)
        .with_json_body(&body)
        .map_err(|e| format!("Failed to serialize JSON body: {}", e))?
        .with_timeout(ALMA_REST_TIMEOUT)
        .await
        .map_err(format_alma_http_error)?;
    read_json_response(conn).await
}

async fn read_json_response(mut conn: trillium_client::Conn) -> Result<(u16, Value), String> {
    let status = conn.status().map(u16::from).unwrap_or(0);
    let body_text = conn
        .response_body()
        .read_string()
        .await
        .map_err(|e| format!("Failed to read Alma response body: {}", e))?;
    let body = if body_text.trim().is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&body_text).unwrap_or(Value::Null)
    };
    Ok((status, body))
}

fn format_alma_http_error(error: trillium_client::Error) -> String {
    let message = error.to_string();
    let lower = message.to_ascii_lowercase();
    if lower.contains("timeout") || lower.contains("timed out") {
        format!(
            "HTTP request timed out after {}s",
            ALMA_REST_TIMEOUT.as_secs()
        )
    } else {
        format!("HTTP request failed: {}", message)
    }
}

/// Create a new thread via Alma REST API. Returns the thread ID.
pub async fn create_thread(state: &SharedState, title: &str) -> Result<String, String> {
    let url = format!("{}/api/threads", state.config.read().await.alma_api);

    let (status, body) = post_json(state, &url, json!({"title": title}))
        .await
        .map_err(|e| {
            if e.contains("timed out") {
                format!(
                    "Create thread timed out after {}s",
                    ALMA_REST_TIMEOUT.as_secs()
                )
            } else {
                e
            }
        })?;

    if !(200..300).contains(&status) {
        return Err(format!("Alma API returned status {}", status));
    }

    let thread_id = body
        .get("id")
        .and_then(|id| id.as_str())
        .ok_or_else(|| "Response missing 'id' field".to_string())?
        .to_string();

    debug!("Created Alma thread: '{}' → {}", title, thread_id);
    Ok(thread_id)
}

/// Fetch the currently selected model for an Alma thread.
///
/// This reflects thread-level model changes made in the Alma GUI.
pub async fn fetch_thread_model(
    state: &SharedState,
    thread_id: &str,
) -> Result<Option<String>, String> {
    let url = format!(
        "{}/api/threads/{}",
        state.config.read().await.alma_api,
        thread_id
    );

    let (status, body) = get_json(state, &url).await.map_err(|e| {
        if e.contains("timed out") {
            format!(
                "Fetch thread {} timed out after {}s",
                thread_id,
                ALMA_REST_TIMEOUT.as_secs()
            )
        } else {
            format!("Failed to fetch thread {}: {}", thread_id, e)
        }
    })?;

    if !(200..300).contains(&status) {
        return Err(format!(
            "Thread API returned status {} for {}",
            status, thread_id
        ));
    }

    Ok(body
        .get("model")
        .and_then(|m| m.as_str())
        .filter(|m| !m.is_empty())
        .map(ToString::to_string))
}

/// Fetch the default model from Alma settings API.
///
/// Returns the model string (e.g. "anthropic:claude-sonnet-4-20250514").
pub async fn fetch_default_model(state: &SharedState) -> Result<String, String> {
    let url = format!("{}/api/settings", state.config.read().await.alma_api);

    let (status, body) = get_json(state, &url).await.map_err(|e| {
        if e.contains("timed out") {
            format!(
                "Fetch settings timed out after {}s",
                ALMA_REST_TIMEOUT.as_secs()
            )
        } else {
            format!("Failed to fetch settings: {}", e)
        }
    })?;

    if !(200..300).contains(&status) {
        return Err(format!("Settings API returned status {}", status));
    }

    let model = body
        .get("chat")
        .and_then(|c| c.get("defaultModel"))
        .and_then(|m| m.as_str())
        .ok_or_else(|| "Settings missing 'chat.defaultModel' field".to_string())?
        .to_string();

    info!("Alma default model: {}", model);
    Ok(model)
}

/// Check whether a thread ID exists through Alma's public REST API.
pub async fn thread_exists(state: &SharedState, thread_id: &str) -> Result<bool, String> {
    let url = format!(
        "{}/api/threads/{}",
        state.config.read().await.alma_api,
        thread_id
    );

    let (status, _body) = get_json(state, &url).await.map_err(|e| {
        if e.contains("timed out") {
            format!(
                "Thread {} existence check timed out after {}s",
                thread_id,
                ALMA_REST_TIMEOUT.as_secs()
            )
        } else {
            format!("Failed to fetch thread {}: {}", thread_id, e)
        }
    })?;

    if (200..300).contains(&status) {
        return Ok(true);
    }

    if status == 404 {
        return Ok(false);
    }

    Err(format!(
        "Thread API returned status {} for {}",
        status, thread_id
    ))
}
