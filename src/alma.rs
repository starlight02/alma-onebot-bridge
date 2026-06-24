use serde_json::json;
use std::time::Duration;
use tokio::time::timeout;
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

/// Create a new thread via Alma REST API. Returns the thread ID.
pub async fn create_thread(state: &SharedState, title: &str) -> Result<String, String> {
    let url = format!("{}/api/threads", state.config.read().await.alma_api);

    let resp = timeout(
        ALMA_REST_TIMEOUT,
        state
            .http_client
            .post(&url)
            .json(&json!({"title": title}))
            .send(),
    )
    .await
    .map_err(|_| {
        format!(
            "Create thread timed out after {}s",
            ALMA_REST_TIMEOUT.as_secs()
        )
    })?
    .map_err(|e| format!("HTTP request failed: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!(
            "Alma API returned status {}",
            resp.status().as_u16()
        ));
    }

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse response: {}", e))?;

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

    let resp = timeout(ALMA_REST_TIMEOUT, state.http_client.get(&url).send())
        .await
        .map_err(|_| {
            format!(
                "Fetch thread {} timed out after {}s",
                thread_id,
                ALMA_REST_TIMEOUT.as_secs()
            )
        })?
        .map_err(|e| format!("Failed to fetch thread {}: {}", thread_id, e))?;

    if !resp.status().is_success() {
        return Err(format!(
            "Thread API returned status {} for {}",
            resp.status().as_u16(),
            thread_id
        ));
    }

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse thread {} response: {}", thread_id, e))?;

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

    let resp = timeout(ALMA_REST_TIMEOUT, state.http_client.get(&url).send())
        .await
        .map_err(|_| {
            format!(
                "Fetch settings timed out after {}s",
                ALMA_REST_TIMEOUT.as_secs()
            )
        })?
        .map_err(|e| format!("Failed to fetch settings: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!(
            "Settings API returned status {}",
            resp.status().as_u16()
        ));
    }

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse settings: {}", e))?;

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

    let resp = timeout(ALMA_REST_TIMEOUT, state.http_client.get(&url).send())
        .await
        .map_err(|_| {
            format!(
                "Thread {} existence check timed out after {}s",
                thread_id,
                ALMA_REST_TIMEOUT.as_secs()
            )
        })?
        .map_err(|e| format!("Failed to fetch thread {}: {}", thread_id, e))?;

    if resp.status().is_success() {
        return Ok(true);
    }

    if resp.status().as_u16() == 404 {
        return Ok(false);
    }

    Err(format!(
        "Thread API returned status {} for {}",
        resp.status().as_u16(),
        thread_id
    ))
}
