//! 报文层数据载体：`RawMessage` / `RawChunk` 及其判定结果。
//!
//! 这些类型表示「协议转换最外层」的原始 HTTP 报文，供 Lua 插件的报文层钩子
//! 直接读写 headers / body / SSE chunk。Rust 内部用 tagged enum 保证 serde
//! 无歧义；plugin crate 负责把它们转换为 Lua 友好的 table 形态。

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 原始报文体。以 `kind` 判别，避免 untagged 在 JSON array 与 binary 间的歧义。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RawBody {
    /// JSON 体（绝大多数 LLM API）。
    Json { value: Value },
    /// 文本体。
    Text { text: String },
    /// 二进制体（base64 传输，Lua 侧以字符串呈现）。
    Binary { data: Vec<u8> },
    /// 空体。
    Empty,
}

impl RawBody {
    /// 从 JSON 值构造。
    pub fn json(value: Value) -> Self {
        RawBody::Json { value }
    }
    /// 从文本构造。
    pub fn text(text: impl Into<String>) -> Self {
        RawBody::Text { text: text.into() }
    }
    /// 引用内部 JSON 值。
    pub fn as_json(&self) -> Option<&Value> {
        match self {
            RawBody::Json { value } => Some(value),
            _ => None,
        }
    }
    /// 可变引用内部 JSON 值。
    pub fn as_json_mut(&mut self) -> Option<&mut Value> {
        match self {
            RawBody::Json { value } => Some(value),
            _ => None,
        }
    }
    /// 引用内部文本。
    pub fn as_text(&self) -> Option<&str> {
        match self {
            RawBody::Text { text } => Some(text),
            _ => None,
        }
    }
    /// 估算体积（字节），用于沙箱配额判断。
    pub fn approx_len(&self) -> usize {
        match self {
            RawBody::Json { value } => value.to_string().len(),
            RawBody::Text { text } => text.len(),
            RawBody::Binary { data } => data.len(),
            RawBody::Empty => 0,
        }
    }
}

/// 非流式报文所处阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RawStage {
    /// 入站请求：客户端 → 网关。
    ClientRequest,
    /// 出站请求：网关 → 上游。
    UpstreamRequest,
    /// 入站响应：上游 → 网关。
    UpstreamResponse,
    /// 出站响应：网关 → 客户端。
    ClientResponse,
}

/// 流式 chunk 所处阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChunkStage {
    /// 上游 SSE chunk。
    UpstreamChunk,
    /// 回写客户端的 SSE chunk。
    ClientChunk,
}

/// 完整 HTTP 报文（非流式四个阶段共用同一载体）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawMessage {
    pub stage: RawStage,
    pub protocol: moonbridge_core::Protocol,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    /// 保序 header 列表，可读写。
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    #[serde(default = "default_body")]
    pub body: RawBody,
}

fn default_body() -> RawBody {
    RawBody::Empty
}

impl RawMessage {
    /// 大小写不敏感读取首个匹配 header。
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
    /// 设置 header（存在则覆盖首个匹配，否则追加）。
    pub fn set_header(&mut self, name: &str, value: impl Into<String>) {
        let value = value.into();
        if let Some(slot) = self
            .headers
            .iter_mut()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
        {
            slot.1 = value;
        } else {
            self.headers.push((name.to_string(), value));
        }
    }
    /// 移除所有匹配 header。
    pub fn remove_header(&mut self, name: &str) {
        self.headers.retain(|(k, _)| !k.eq_ignore_ascii_case(name));
    }
}

/// 单个 SSE chunk（流式）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawChunk {
    pub stage: ChunkStage,
    pub protocol: moonbridge_core::Protocol,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// SSE `event:` 字段。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
    /// SSE `data:` 字段解析结果（通常为 JSON）。
    #[serde(default = "default_body")]
    pub data: RawBody,
    /// 原始 `data:` 行文本（当 data 非 JSON 或需保真时使用）。
    #[serde(default)]
    pub raw: String,
}

impl RawChunk {
    /// 构造一个 JSON data 的 chunk。
    pub fn json(
        stage: ChunkStage,
        protocol: moonbridge_core::Protocol,
        event: Option<String>,
        value: Value,
    ) -> Self {
        let raw = value.to_string();
        RawChunk {
            stage,
            protocol,
            provider: None,
            event,
            data: RawBody::json(value),
            raw,
        }
    }
}

/// 报文层钩子对完整报文的处置判定。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum RawVerdict {
    /// 放行（使用就地修改后的报文继续）。
    Pass,
    /// 短路：网关直接用给定响应应答客户端，跳过上游。
    ShortCircuit {
        status: u16,
        #[serde(default)]
        headers: Vec<(String, String)>,
        #[serde(default = "default_body")]
        body: RawBody,
    },
    /// 中止：返回错误给客户端。
    Abort { message: String },
}

/// 报文层钩子对流式 chunk 的处置判定。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChunkVerdict {
    /// 转发（使用就地修改后的 chunk）。
    Forward,
    /// 丢弃该 chunk。
    Drop,
}
