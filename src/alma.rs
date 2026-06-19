use serde_json::json;
use tracing::{debug, info};

use crate::state::SharedState;

/// Create a new thread via Alma REST API. Returns the thread ID.
pub async fn create_thread(state: &SharedState, title: &str) -> Result<String, String> {
    let url = format!("{}/api/threads", state.config.alma_api);

    let resp = state
        .http_client
        .post(&url)
        .json(&json!({"title": title}))
        .send()
        .await
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
    let url = format!("{}/api/threads/{}", state.config.alma_api, thread_id);

    let resp = state
        .http_client
        .get(&url)
        .send()
        .await
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
    let url = format!("{}/api/settings", state.config.alma_api);

    let resp = state
        .http_client
        .get(&url)
        .send()
        .await
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
