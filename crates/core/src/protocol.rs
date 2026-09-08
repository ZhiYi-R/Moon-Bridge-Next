//! `Protocol` 枚举：标识入口协议（客户端 → 网关）与上游协议（网关 → provider）。
//!
//! serde 值与 Moon Bridge 的 `protocol` 字段对齐（kebab/lower 混合格式），
//! 便于配置迁移与生态兼容。

use serde::{Deserialize, Serialize};

/// 支持的协议种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Protocol {
    /// OpenAI Responses API（`/v1/responses`）。
    #[serde(rename = "openai-response")]
    OpenAiResponse,
    /// Anthropic Messages API（`/v1/messages`）。
    #[serde(rename = "anthropic")]
    Anthropic,
    /// OpenAI Chat Completions API（`/v1/chat/completions`）。
    #[serde(rename = "openai-chat")]
    OpenAiChat,
    /// Google Generative AI (Gemini) API。
    #[serde(rename = "google-genai")]
    GoogleGenai,
}

impl Protocol {
    /// 协议的稳定字符串标识（与 serde rename 一致）。
    pub fn as_str(&self) -> &'static str {
        match self {
            Protocol::OpenAiResponse => "openai-response",
            Protocol::Anthropic => "anthropic",
            Protocol::OpenAiChat => "openai-chat",
            Protocol::GoogleGenai => "google-genai",
        }
    }

    /// 从字符串解析协议；接受 serde 标识及若干常见别名（含下划线变体）。
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "openai-response"
            | "openai-responses"
            | "openai_response"
            | "openai_responses"
            | "responses"
            | "response" => Some(Protocol::OpenAiResponse),
            "anthropic" | "anthropic-messages" | "messages" | "claude" => Some(Protocol::Anthropic),
            "openai-chat"
            | "openai_chat"
            | "chat"
            | "chat-completions"
            | "chat_completions"
            | "openai" => Some(Protocol::OpenAiChat),
            "google-genai" | "google_genai" | "gemini" | "google" | "genai" => {
                Some(Protocol::GoogleGenai)
            }
            _ => None,
        }
    }

    /// 全部协议（用于枚举/注册表初始化）。
    pub fn all() -> [Protocol; 4] {
        [
            Protocol::OpenAiResponse,
            Protocol::Anthropic,
            Protocol::OpenAiChat,
            Protocol::GoogleGenai,
        ]
    }
}

impl std::fmt::Display for Protocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_display() {
        assert_eq!(Protocol::parse("anthropic"), Some(Protocol::Anthropic));
        assert_eq!(Protocol::parse("Claude"), Some(Protocol::Anthropic));
        assert_eq!(Protocol::parse("openai-chat"), Some(Protocol::OpenAiChat));
        assert_eq!(Protocol::parse("openai_chat"), Some(Protocol::OpenAiChat));
        assert_eq!(Protocol::parse("openai_response"), Some(Protocol::OpenAiResponse));
        assert_eq!(Protocol::parse("google-genai"), Some(Protocol::GoogleGenai));
        assert_eq!(Protocol::parse("gemini"), Some(Protocol::GoogleGenai));
        assert_eq!(Protocol::parse("bogus"), None);
        assert_eq!(Protocol::OpenAiResponse.to_string(), "openai-response");
    }

    #[test]
    fn serde_roundtrip() {
        let json = serde_json::to_string(&Protocol::GoogleGenai).unwrap();
        assert_eq!(json, r#""google-genai""#);
        let back: Protocol = serde_json::from_str(&json).unwrap();
        assert_eq!(back, Protocol::GoogleGenai);
    }
}
