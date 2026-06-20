use serde::{Deserialize, Serialize};

// ─── OneBot v11 Event Types ──────────────────────────────────────────────────

/// Top-level OneBot event. `self_id` is always present; the rest depends on `post_type`.
#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
pub struct OneBotEvent {
    pub time: Option<u64>,
    pub self_id: i64,
    pub post_type: String,

    // Message fields
    pub message_type: Option<String>,
    pub sub_type: Option<String>,
    pub message_id: Option<i64>,
    pub user_id: Option<i64>,
    pub group_id: Option<i64>,
    pub message: Option<Vec<MessageSegment>>,
    pub raw_message: Option<String>,
    pub sender: Option<Sender>,

    // Meta event
    pub meta_event_type: Option<String>,

    // Notice
    pub notice_type: Option<String>,
    pub operator_id: Option<i64>,

    // Request
    pub request_type: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
pub struct Sender {
    pub user_id: Option<i64>,
    pub nickname: Option<String>,
    pub sex: Option<String>,
    pub age: Option<i32>,
    pub card: Option<String>,
    pub role: Option<String>,
    pub title: Option<String>,
}

// ─── Message Segments (Array format) ─────────────────────────────────────────

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct MessageSegment {
    #[serde(rename = "type")]
    pub segment_type: String,
    #[serde(default)]
    pub data: serde_json::Value,
}

#[allow(dead_code)]
impl MessageSegment {
    pub fn text(content: &str) -> Self {
        MessageSegment {
            segment_type: "text".to_string(),
            data: serde_json::json!({"text": content}),
        }
    }

    pub fn at(qq: &str) -> Self {
        MessageSegment {
            segment_type: "at".to_string(),
            data: serde_json::json!({"qq": qq}),
        }
    }

    pub fn reply(message_id: &str) -> Self {
        MessageSegment {
            segment_type: "reply".to_string(),
            data: serde_json::json!({"id": message_id}),
        }
    }
}

// ─── API Request / Response ──────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct ApiRequest {
    pub action: String,
    pub params: serde_json::Value,
    pub echo: String,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct ApiResponse {
    pub status: String,
    pub retcode: i32,
    pub data: Option<serde_json::Value>,
    pub echo: Option<String>,
}

// ─── Helper functions ────────────────────────────────────────────────────────

fn value_as_string(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Extract plain text from a message segment array.
pub fn extract_text(segments: &[MessageSegment]) -> String {
    segments
        .iter()
        .filter(|seg| seg.segment_type == "text")
        .filter_map(|seg| seg.data.get("text").and_then(|t| t.as_str()))
        .collect::<Vec<_>>()
        .join("")
        .trim()
        .to_string()
}

/// Check whether the message contains an @mention of the bot.
pub fn contains_at_bot(segments: &[MessageSegment], bot_id: i64) -> bool {
    let bot_id_str = bot_id.to_string();
    segments.iter().any(|seg| {
        seg.segment_type == "at"
            && seg
                .data
                .get("qq")
                .and_then(value_as_string)
                .map(|q| q == bot_id_str || q == "all")
                .unwrap_or(false)
    })
}

/// Remove @bot mentions from text content, returning cleaned text.
pub fn clean_at_from_text(text: &str, bot_id: i64) -> String {
    let bot_id_str = bot_id.to_string();
    let patterns = [
        format!("[CQ:at,qq={}]", bot_id_str),
        format!("[CQ:at,qq={},name=.*?]", bot_id_str),
    ];
    let mut result = text.to_string();
    for pat in &patterns {
        result = result.replace(pat, "");
    }
    result.trim().to_string()
}

/// Extract the message ID from a reply segment (first element in the array).
/// Returns None if no reply segment is present.
pub fn extract_reply_id(segments: &[MessageSegment]) -> Option<String> {
    segments.first().and_then(|seg| {
        if seg.segment_type == "reply" {
            seg.data.get("id").and_then(value_as_string)
        } else {
            None
        }
    })
}

/// Convert face segments to human-readable text like `[emoji:斜眼笑]`.
/// Returns an empty string if no face segments are found.
pub fn convert_faces_to_text(segments: &[MessageSegment]) -> String {
    let faces: Vec<String> = segments
        .iter()
        .filter(|seg| seg.segment_type == "face")
        .filter_map(|seg| {
            seg.data
                .get("id")
                .and_then(value_as_string)
                .map(|id| match crate::face_map::face_name(&id) {
                    Some(name) => format!("[emoji:{}]", name),
                    None => format!("[emoji:face_{}]", id),
                })
        })
        .collect();

    faces.join(" ")
}

/// Extract image URLs from image segments.
pub fn extract_images(segments: &[MessageSegment]) -> Vec<String> {
    segments
        .iter()
        .filter(|seg| seg.segment_type == "image")
        .filter_map(|seg| {
            seg.data
                .get("url")
                .and_then(|u| u.as_str())
                .map(|u| u.to_string())
        })
        .collect()
}

/// Summarize non-text media segments in the message.
/// Returns lines like "[Image]", "[Voice message]", "[Video]", "[Flash image]".
pub fn extract_media_summary(segments: &[MessageSegment]) -> Vec<String> {
    segments
        .iter()
        .filter_map(|seg| match seg.segment_type.as_str() {
            "image" => {
                let is_flash = seg
                    .data
                    .get("type")
                    .and_then(|t| t.as_str())
                    .map(|t| t == "flash")
                    .unwrap_or(false);
                if is_flash {
                    Some("[Flash image]".to_string())
                } else {
                    Some("[Image]".to_string())
                }
            }
            "record" => Some("[Voice message]".to_string()),
            "video" => Some("[Video]".to_string()),
            "share" => {
                let url = seg.data.get("url").and_then(|u| u.as_str()).unwrap_or("");
                let title = seg
                    .data
                    .get("title")
                    .and_then(|t| t.as_str())
                    .unwrap_or("shared link");
                Some(format!("[Share: {} — {}]", title, url))
            }
            "location" => {
                let lat = seg.data.get("lat").and_then(|l| l.as_str()).unwrap_or("?");
                let lon = seg.data.get("lon").and_then(|l| l.as_str()).unwrap_or("?");
                Some(format!("[Location: {}, {}]", lat, lon))
            }
            // text, at, reply, face, forward — handled elsewhere
            _ => None,
        })
        .collect()
}

/// Check if a message has any of the specified media segment types.
pub fn has_media_segments(segments: &[MessageSegment]) -> bool {
    segments.iter().any(|seg| {
        matches!(
            seg.segment_type.as_str(),
            "image" | "record" | "video" | "share" | "location" | "forward" | "file"
        )
    })
}

/// Extract the forward ID from a forward segment, if present.
pub fn extract_forward_id(segments: &[MessageSegment]) -> Option<String> {
    segments.iter().find_map(|seg| {
        if seg.segment_type == "forward" {
            seg.data.get("id").and_then(value_as_string)
        } else {
            None
        }
    })
}

/// Extract file segments. Returns a list of (filename, url).
pub fn extract_files(segments: &[MessageSegment]) -> Vec<(String, String)> {
    segments
        .iter()
        .filter(|seg| seg.segment_type == "file")
        .filter_map(|seg| {
            let url = seg.data.get("url").and_then(|u| u.as_str()).unwrap_or("");
            let name = seg
                .data
                .get("file")
                .or_else(|| seg.data.get("name"))
                .and_then(|n| n.as_str())
                .unwrap_or("file");
            if url.is_empty() {
                None
            } else {
                Some((name.to_string(), url.to_string()))
            }
        })
        .collect()
}

/// Convert text containing `[emoji:NAME]` patterns into a segment array.
/// Recognized emoji names become `face` segments; everything else stays as `text`.
pub fn text_to_segments(text: &str) -> Vec<MessageSegment> {
    let mut segments = Vec::new();
    let mut remaining = text;

    while let Some(start) = remaining.find("[emoji:") {
        if let Some(end) = remaining[start..].find(']') {
            let name = &remaining[start + 7..start + end];

            // Flush text before this emoji tag
            if start > 0 {
                let before = &remaining[..start];
                if !before.is_empty() {
                    segments.push(MessageSegment::text(before));
                }
            }

            // Try reverse lookup: name → face ID
            if let Some(id) = crate::face_map::face_id(name) {
                segments.push(MessageSegment {
                    segment_type: "face".to_string(),
                    data: serde_json::json!({"id": id}),
                });
            } else {
                // Unknown name — keep as-is
                segments.push(MessageSegment::text(&remaining[start..start + end + 1]));
            }

            remaining = &remaining[start + end + 1..];
        } else {
            break; // No closing bracket, treat rest as text
        }
    }

    // Flush remaining text
    if !remaining.is_empty() {
        segments.push(MessageSegment::text(remaining));
    }

    segments
}

#[cfg(test)]
mod tests {
    use super::{
        MessageSegment, contains_at_bot, convert_faces_to_text, extract_forward_id,
        extract_reply_id,
    };

    #[test]
    fn contains_at_bot_accepts_numeric_qq_field() {
        let segments = vec![MessageSegment {
            segment_type: "at".to_string(),
            data: serde_json::json!({"qq": 123456}),
        }];

        assert!(contains_at_bot(&segments, 123456));
    }

    #[test]
    fn extract_reply_id_accepts_numeric_id_field() {
        let segments = vec![MessageSegment {
            segment_type: "reply".to_string(),
            data: serde_json::json!({"id": 67890}),
        }];

        assert_eq!(extract_reply_id(&segments).as_deref(), Some("67890"));
    }

    #[test]
    fn extract_forward_id_accepts_numeric_id_field() {
        let segments = vec![MessageSegment {
            segment_type: "forward".to_string(),
            data: serde_json::json!({"id": 13579}),
        }];

        assert_eq!(extract_forward_id(&segments).as_deref(), Some("13579"));
    }

    #[test]
    fn convert_faces_to_text_accepts_numeric_face_id() {
        let segments = vec![MessageSegment {
            segment_type: "face".to_string(),
            data: serde_json::json!({"id": 178}),
        }];

        assert_eq!(convert_faces_to_text(&segments), "[emoji:斜眼笑]");
    }
}
