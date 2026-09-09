//! 协议中立的 Core IR（中间表示）。
//!
//! 所有 Adapter 在「入口/上游协议 DTO」与「Core IR」之间转换。Core IR 使用
//! Rust enum + serde（`#[serde(tag = "type")]`）建模内容块与流事件，既类型安全，
//! 又能经 mlua 的 serde 集成自动与 Lua table 互转，供 Lua 插件读写。

use serde::{Deserialize, Serialize};
use serde_json::{Map as JsonMap, Value};

/// 通用 JSON 对象映射（保序，依赖 serde_json 的 `preserve_order` 特性）。
pub type Map = JsonMap<String, Value>;

// ============================================================================
// Content Block
// ============================================================================

/// 消息中的单个内容块。以 `type` 字段判别具体种类。
///
/// 对应各协议：
/// - Anthropic：`text` / `image` / `tool_use` / `tool_result` / `thinking`
/// - OpenAI Responses：`output_text` / `function_call` / `function_call_output` / `reasoning`
/// - OpenAI Chat：`text` / `image_url` / `tool_calls` / `tool` role
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    /// 纯文本。
    Text { text: String },
    /// 图像（base64 数据 + MIME 类型）。
    Image {
        #[serde(rename = "image_data")]
        data: String,
        media_type: String,
    },
    /// 模型发起的工具调用。
    ToolUse {
        id: String,
        name: String,
        /// namespace 工具（如 Codex `multi_agent_v1`）的 wrapper 名，可为空。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        namespace: Option<String>,
        /// 工具入参（原始 JSON）。
        #[serde(default)]
        input: Value,
        /// 回传凭据：Gemini 2.5 functionCall part 的 `thoughtSignature`（与
        /// function call 强绑定的加密 CoT 凭据，多轮回传缺失会被拒或退化）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    /// 工具调用结果。
    ToolResult {
        tool_use_id: String,
        content: Vec<ContentBlock>,
        #[serde(default)]
        is_error: bool,
    },
    /// 推理/思考块（Anthropic thinking、OpenAI reasoning 的中间表示）。
    Reasoning {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
}

impl ContentBlock {
    /// 便捷构造文本块。
    pub fn text(text: impl Into<String>) -> Self {
        ContentBlock::Text { text: text.into() }
    }

    /// 返回该块的 `type` 判别字符串（用于日志/调试）。
    pub fn kind(&self) -> &'static str {
        match self {
            ContentBlock::Text { .. } => "text",
            ContentBlock::Image { .. } => "image",
            ContentBlock::ToolUse { .. } => "tool_use",
            ContentBlock::ToolResult { .. } => "tool_result",
            ContentBlock::Reasoning { .. } => "reasoning",
        }
    }
}

// ============================================================================
// Message / Role
// ============================================================================

/// 会话角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// 单条消息。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    #[serde(default)]
    pub content: Vec<ContentBlock>,
    /// 协议特定扩展字段（缓存控制、provider hint 等）。
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub ext: Map,
}

impl Message {
    /// 便捷构造一条纯文本消息。
    pub fn text(role: Role, text: impl Into<String>) -> Self {
        Message {
            role,
            content: vec![ContentBlock::text(text)],
            ext: Map::new(),
        }
    }
}

// ============================================================================
// Tool
// ============================================================================

/// 模型可调用的工具定义。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tool {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON Schema 描述的入参结构。
    #[serde(default)]
    pub input_schema: Value,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub ext: Map,
}

/// 工具选择策略。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolChoice {
    Auto,
    None,
    Required,
    /// 强制调用指定工具。
    Tool { name: String },
}

// ============================================================================
// Reasoning / StopReason / Usage
// ============================================================================

/// 推理配置（对应 OpenAI reasoning、Anthropic thinking budget）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reasoning {
    /// 推理强度：`low` / `medium` / `high` / `xhigh` / `max` 等。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// 推理摘要配置（原始 JSON，协议特定）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<Value>,
}

/// 停止原因（协议中立）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    StopSequence,
    ToolUse,
    ContentFilter,
}

/// token 用量统计。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u32,
    #[serde(default)]
    pub output_tokens: u32,
    #[serde(default)]
    pub cache_read_tokens: u32,
    #[serde(default)]
    pub cache_write_tokens: u32,
    /// 推理 token（OpenAI reasoning / Gemini thoughts）；无此概念的协议恒为 0。
    #[serde(default)]
    pub reasoning_tokens: u32,
}

// ============================================================================
// CoreRequest / CoreResponse
// ============================================================================

/// 协议中立的请求中间表示。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoreRequest {
    /// 上游模型名（路由解析后的实际模型）。
    pub model: String,
    /// 客户端请求的模型别名（路由前）。
    #[serde(default)]
    pub model_alias: String,
    /// system 指令（独立于 messages，便于 Anthropic/OpenAI 各自映射）。
    #[serde(default)]
    pub system: Vec<ContentBlock>,
    #[serde(default)]
    pub messages: Vec<Message>,
    #[serde(default)]
    pub tools: Vec<Tool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(default)]
    pub stop: Vec<String>,
    #[serde(default)]
    pub stream: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<Reasoning>,
    /// 请求级元数据：session_id、原始 headers、客户端标识等。
    #[serde(default)]
    pub meta: Map,
}

impl CoreRequest {
    /// 构造一个最小请求。
    pub fn new(model: impl Into<String>) -> Self {
        CoreRequest {
            model: model.into(),
            model_alias: String::new(),
            system: Vec::new(),
            messages: Vec::new(),
            tools: Vec::new(),
            tool_choice: None,
            max_tokens: None,
            temperature: None,
            top_p: None,
            stop: Vec::new(),
            stream: false,
            reasoning: None,
            meta: Map::new(),
        }
    }

    /// 从 `meta` 读取 session_id。
    pub fn session_id(&self) -> Option<&str> {
        self.meta.get("session_id").and_then(|v| v.as_str())
    }
}

/// 协议中立的响应中间表示（非流式）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoreResponse {
    pub id: String,
    pub model: String,
    #[serde(default)]
    pub content: Vec<ContentBlock>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<StopReason>,
    #[serde(default)]
    pub usage: Usage,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub ext: Map,
}

// ============================================================================
// Core stream events
// ============================================================================

/// 流式增量片段。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamDelta {
    /// 文本增量。
    Text { text: String },
    /// 推理文本增量。
    Reasoning { text: String },
    /// 推理凭据增量：加密 CoT 的回传凭据（如 anthropic `signature_delta`、
    /// responses reasoning item 的 `encrypted_content`）。不透明文本，
    /// 仅供入口协议原样透传给客户端供下一轮回传，绝不是展示内容。
    ReasoningSignature { signature: String },
    /// 工具入参增量（部分 JSON 字符串）。
    ToolInput { partial_json: String },
}

/// 协议中立的流事件。上游 Adapter 把 SSE 解码为 `CoreStreamEvent`，
/// 入口 Adapter 再把 `CoreStreamEvent` 编码为客户端协议的 SSE。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CoreStreamEvent {
    MessageStart {
        id: String,
        model: String,
    },
    BlockStart {
        index: usize,
        block: ContentBlock,
    },
    BlockDelta {
        index: usize,
        delta: StreamDelta,
    },
    BlockStop {
        index: usize,
        /// 结束的块（decode 端可知时提供）。Responses 入口的收尾事件形态
        /// 依赖它区分 reasoning/message；其余入口忽略。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        block: Option<ContentBlock>,
    },
    MessageDelta {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stop_reason: Option<StopReason>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<Usage>,
    },
    MessageStop,
    Error {
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_block_text_roundtrip() {
        let block = ContentBlock::text("hello");
        let json = serde_json::to_string(&block).unwrap();
        assert_eq!(json, r#"{"type":"text","text":"hello"}"#);
        let back: ContentBlock = serde_json::from_str(&json).unwrap();
        assert_eq!(back, block);
    }

    #[test]
    fn content_block_tool_use_roundtrip() {
        let block = ContentBlock::ToolUse {
            id: "call_1".into(),
            name: "get_time".into(),
            namespace: None,
            input: serde_json::json!({"tz": "UTC"}),
            signature: None,
        };
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json["type"], "tool_use");
        let back: ContentBlock = serde_json::from_value(json).unwrap();
        assert_eq!(back, block);
    }

    #[test]
    fn core_request_defaults() {
        let req = CoreRequest::new("deepseek-v4-pro");
        let json = serde_json::to_value(&req).unwrap();
        // 空字段应被 default/skip 处理，反序列化可还原
        let back: CoreRequest = serde_json::from_value(json).unwrap();
        assert_eq!(back.model, "deepseek-v4-pro");
        assert!(!back.stream);
    }

    #[test]
    fn stream_event_roundtrip() {
        let ev = CoreStreamEvent::BlockDelta {
            index: 0,
            delta: StreamDelta::Text { text: "hi".into() },
        };
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["type"], "block_delta");
        assert_eq!(json["delta"]["type"], "text");
        let back: CoreStreamEvent = serde_json::from_value(json).unwrap();
        assert_eq!(back, ev);
    }
}
