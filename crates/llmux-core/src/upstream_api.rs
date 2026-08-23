use serde::{Deserialize, Serialize};

/// Upstream API preference for an alias/aggregate alias.
/// Pure config — no model-name hardcoding. Defaults to Chat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpstreamApi {
    Chat,
    Responses,
    Messages,
    #[deprecated(note = "Auto is deprecated, use Chat or Default via protocol::DownstreamMode instead")]
    Auto,
}

impl UpstreamApi {
    pub fn from_str(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "responses" => Self::Responses,
            "messages" => Self::Messages,
            "auto" => {
                // Auto now maps to Chat (Default) for backwards compat; callers should use protocol::DownstreamMode::from_str for Default semantics.
                Self::Chat
            }
            _ => Self::Chat,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Responses => "responses",
            Self::Messages => "messages",
            Self::Auto => "auto",
        }
    }

    pub fn wants_responses(self) -> bool {
        matches!(self, Self::Responses | Self::Auto)
    }
}

impl Default for UpstreamApi {
    fn default() -> Self {
        Self::Chat
    }
}

impl std::fmt::Display for UpstreamApi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
