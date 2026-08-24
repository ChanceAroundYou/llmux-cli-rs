use crate::adapters::Account;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Chat,
    Responses,
    Messages,
}

impl Protocol {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Responses => "responses",
            Self::Messages => "messages",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "responses" => Self::Responses,
            "messages" => Self::Messages,
            _ => Self::Chat,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownstreamMode {
    Default,
    Chat,
    Responses,
    Messages,
}

impl DownstreamMode {
    pub fn from_str(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "chat" => Self::Chat,
            "responses" => Self::Responses,
            "messages" => Self::Messages,
            "auto" | "default" | "" => Self::Default,
            _ => Self::Default,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Chat => "chat",
            Self::Responses => "responses",
            Self::Messages => "messages",
        }
    }
}

pub fn supports(a: &Account, p: Protocol) -> bool {
    endpoint_for(a, p).is_some()
}

pub fn endpoint_for(a: &Account, p: Protocol) -> Option<&str> {
    // Since 0012 every write keeps base_url = chat_endpoint, so base_url is the
    // chat source of truth; chat_endpoint remains as legacy-compat fallback
    // (e.g. clients explicitly clearing base_url while keeping chat_endpoint).
    match p {
        Protocol::Chat => a
            .base_url
            .as_deref()
            .filter(|s| !s.is_empty())
            .or_else(|| a.chat_endpoint.as_deref().filter(|s| !s.is_empty())),
        Protocol::Responses => a
            .responses_endpoint
            .as_deref()
            .filter(|s| !s.is_empty())
            .or_else(|| a.base_url.as_deref().filter(|s| !s.is_empty())),
        // Messages deliberately does NOT fall back to base_url: base_url is an
        // OpenAI-compatible endpoint (e.g. opencode.ai/zen/go/v1) whose /v1/messages
        // Anthropic route often doesn't exist or is broken. Accounts without an
        // explicit Anthropic endpoint are treated as chat-only and their messages
        // ingress gets protocol-converted instead of blindly POSTing to /v1/messages.
        Protocol::Messages => a
            .messages_endpoint
            .as_deref()
            .filter(|s| !s.is_empty())
            .or_else(|| a.anthropic_base_url.as_deref().filter(|s| !s.is_empty())),
    }
}

pub fn target_protocol(
    ingress: Protocol,
    mode: DownstreamMode,
    account: &Account,
) -> Protocol {
    match mode {
        DownstreamMode::Default => {
            if supports(account, ingress) {
                ingress
            } else {
                default_protocol_for(account)
            }
        }
        DownstreamMode::Chat => Protocol::Chat,
        DownstreamMode::Responses => Protocol::Responses,
        DownstreamMode::Messages => Protocol::Messages,
    }
}

pub fn default_protocol_for(a: &Account) -> Protocol {
    match a.default_protocol.as_deref().unwrap_or("chat") {
        "responses" => Protocol::Responses,
        "messages" => Protocol::Messages,
        _ => Protocol::Chat,
    }
}
