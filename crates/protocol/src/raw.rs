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
    Json {
        value: Value,
    },
    Text {
        text: String,
    },
    /// 二进制体（base64 传输，Lua 侧以字符串呈现）。
    Binary {
        data: Vec<u8>,
    },
    Empty,
}

impl RawBody {
    pub fn json(value: Value) -> Self {
        RawBody::Json { value }
    }
    pub fn text(text: impl Into<String>) -> Self {
        RawBody::Text { text: text.into() }
    }
    pub fn as_json(&self) -> Option<&Value> {
        match self {
            RawBody::Json { value } => Some(value),
            _ => None,
        }
    }
    pub fn as_json_mut(&mut self) -> Option<&mut Value> {
        match self {
            RawBody::Json { value } => Some(value),
            _ => None,
        }
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RawStage {
    ClientRequest,
    UpstreamRequest,
    UpstreamResponse,
    ClientResponse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChunkStage {
    UpstreamChunk,
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
    /// 会话身份覆写字段：仅在 `client_request` 阶段可写，非空值将作为最高优先级的会话 ID，
    /// 设为 `nil` 则清除宿主已提取的会话标识。其他阶段仅作只读暴露。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// 上游错误路径标记（只读）：仅在 `upstream_response` 阶段遇到上游非 2xx 或流式零产出失败时为 `true`。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub error: bool,
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
    /// 同名 header 存在时覆盖首个匹配，否则追加。
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
    /// 延迟 `delay_ms` 毫秒后重发当前上游请求。
    ///
    /// 仅在 `on_upstream_response_raw` 的错误路径（`msg.error == true`）生效，且作为终局判定立即触发重试，
    /// 不再执行同阶段后续插件；其他阶段或成功路径返回该值时按 `Pass` 处理。
    /// 重试次数与单次延迟受网关配置 `plugin_retry_max` 与 `plugin_retry_delay_cap_ms` 限制。
    Retry {
        #[serde(default)]
        delay_ms: u64,
    },
    /// 中止：返回错误给客户端。
    Abort { message: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChunkVerdict {
    /// 转发（使用就地修改后的 chunk）。
    Forward,
    Drop,
}
