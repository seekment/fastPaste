//! Core data model shared by storage, index, monitor and UI layers.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContentType {
    Text,
    Rtf,
    Html,
    Link,
    Image,
    Files,
}

impl ContentType {
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0 => Self::Text,
            1 => Self::Rtf,
            2 => Self::Html,
            3 => Self::Link,
            4 => Self::Image,
            5 => Self::Files,
            _ => return None,
        })
    }
    pub fn to_u8(self) -> u8 {
        match self {
            Self::Text => 0,
            Self::Rtf => 1,
            Self::Html => 2,
            Self::Link => 3,
            Self::Image => 4,
            Self::Files => 5,
        }
    }
}

/// Metadata per FR-H03. `preview` is a truncated text used for cards and search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipMeta {
    pub id: u64,
    pub content_type: ContentType,
    pub content_hash: u64,
    pub source_app: String,
    pub created_at: i64, // epoch millis
    pub pinned: bool,
    pub item_file: String,
    pub blob_ref: Option<String>,
    pub thumb_ref: Option<String>,
    pub text_length: usize,
    pub preview: String,
    /// PasteMate-style key binding: pressing this key anywhere pastes the card
    /// without opening the panel. None = unbound.
    #[serde(default)]
    pub bound_key: Option<String>,
}

/// Full payload persisted in .fpitem, loaded only on preview/paste (FR 5.4).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClipPayload {
    pub text: Option<String>,
    pub rtf: Option<String>,
    pub html: Option<String>,
    pub files: Option<Vec<String>>,
    /// Raw image bytes (BMP on Windows, PNG/TIFF on macOS) - kept in blobs/.
    pub image: Option<Vec<u8>>,
}

/// A freshly captured clipboard entry before dedup/storage.
pub struct IncomingClip {
    pub content_type: ContentType,
    pub source_app: String,
    pub text_for_index: String, // text / html-stripped / ocr text / file names
    pub payload: ClipPayload,
    pub image_bytes: Option<Vec<u8>>,
    pub thumbnail: Option<Vec<u8>>,
}

impl IncomingClip {
    pub fn text(s: &str, app: &str) -> Self {
        let ct = if s.trim_start().starts_with("http://") || s.trim_start().starts_with("https://") {
            ContentType::Link
        } else {
            ContentType::Text
        };
        Self {
            content_type: ct,
            source_app: app.to_string(),
            text_for_index: s.to_string(),
            payload: ClipPayload { text: Some(s.to_string()), ..Default::default() },
            image_bytes: None,
            thumbnail: None,
        }
    }
}

pub fn fnv1a(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub fn make_preview(text: &str) -> String {
    let t = text.trim();
    let mut p: String = t.chars().take(200).collect();
    if t.chars().count() > 200 { p.push('…'); }
    p
}

/// Minimal HTML -> plain text (FR-A01): strips tags, decodes common entities.
pub fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">")
        .replace("&quot;", "\"").replace("&#39;", "'").replace("&nbsp;", " ")
}
