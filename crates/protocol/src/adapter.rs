//! Adapter 契约：入口/上游 × 非流式/流式 的四象限接口。
//!
//! 所有 Adapter 以 Core IR 为中心进行转换：入口 Adapter（Client*）在
//! 「客户端协议 ⇄ Core」间转换，上游 Adapter（Provider*）在「Core ⇄ 上游协议」
//! 间转换。这样 N 个入口 + M 个上游只需 N + M 个 Adapter，而非 N × M。

use async_trait::async_trait;
use http::Method;
use moonbridge_core::{CoreRequest, CoreResponse, CoreStreamEvent, Map, Protocol, Result};
use serde_json::Value;

use crate::context::ReqCtx;
use crate::raw::RawChunk;

/// 上游 provider 端点信息，由 gateway 从 store 配置构造后传给 ProviderAdapter。
#[derive(Debug, Clone)]
pub struct ProviderEndpoint {
    /// provider 唯一 key。
    pub key: String,
    /// 上游协议。
    pub protocol: Protocol,
    /// 基础 URL（如 `https://api.anthropic.com`）。
    pub base_url: String,
    /// API Key（已解密）。
    pub api_key: String,
    /// 协议版本头（如 Anthropic 的 `2023-06-01`）。
    pub version: Option<String>,
    /// 自定义 User-Agent。
    pub user_agent: Option<String>,
    /// 协议特定额外字段。
    pub extra: Map,
}

impl ProviderEndpoint {
    /// 构造一个最小端点。
    pub fn new(
        key: impl Into<String>,
        protocol: Protocol,
        base_url: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Self {
        ProviderEndpoint {
            key: key.into(),
            protocol,
            base_url: base_url.into(),
            api_key: api_key.into(),
            version: None,
            user_agent: None,
            extra: Map::new(),
        }
    }
}

/// Adapter 把 CoreRequest 转成上游协议后的「待发送请求」。
#[derive(Debug, Clone)]
pub struct UpstreamRequest {
    pub method: Method,
    pub url: String,
    pub headers: Vec<(String, String)>,
    /// JSON 请求体（绝大多数 LLM API 为 JSON）。
    pub body: Value,
    pub stream: bool,
}

/// 入口协议 ↔ Core（非流式）。例：OpenAI Responses 请求 → CoreRequest。
#[async_trait]
pub trait ClientAdapter: Send + Sync {
    /// 该 Adapter 处理的入口协议。
    fn protocol(&self) -> Protocol;
    /// 客户端原始请求 JSON → CoreRequest。
    async fn to_core_request(&self, ctx: &ReqCtx, raw: Value) -> Result<CoreRequest>;
    /// CoreResponse → 客户端协议响应 JSON。
    //
    // `from_core_*` / `to_core_*` 表达的是 **Core ↔ 协议的转换方向**、与配对方法对称，
    // 不是构造函数；且 Adapter 经 `Arc<dyn …>` 动态派发，`&self` 无法去除。
    #[allow(clippy::wrong_self_convention)]
    async fn from_core_response(&self, ctx: &ReqCtx, resp: CoreResponse) -> Result<Value>;
}

/// Core 流事件 → 入口协议 SSE（流式）。
#[async_trait]
pub trait ClientStreamAdapter: Send + Sync {
    fn protocol(&self) -> Protocol;
    /// 把一个 Core 流事件编码为 0..n 个客户端 SSE chunk。
    ///
    /// gateway 会在写出前对每个 chunk 触发 `on_client_chunk_raw` 钩子。
    /// `st` 为每流状态（gateway 每个流式请求持有一份）：部分协议需跨事件
    /// 决策（如 anthropic 推理块惰性开启，避免产生空 thinking 块）。
    fn encode(
        &self,
        ctx: &ReqCtx,
        ev: &CoreStreamEvent,
        st: &mut StreamEncodeState,
    ) -> Result<Vec<RawChunk>>;
}

/// encode 的每流状态。adapter 本体为并发共享单例，可变状态由 gateway
/// 按请求持有，encode 保持可重入。
#[derive(Default)]
pub struct StreamEncodeState {
    /// 已向客户端开启（content_block_start 已发出）的块索引。
    pub open_blocks: std::collections::HashSet<usize>,
}

/// Core ↔ 上游协议（非流式）。例：CoreRequest → Anthropic Messages 请求。
#[async_trait]
pub trait ProviderAdapter: Send + Sync {
    fn protocol(&self) -> Protocol;
    /// CoreRequest → 上游待发送请求（含 URL/headers/body）。
    ///
    /// 命名口径同上（转换方向，非构造函数；`&self` 为动态派发所必需）。
    #[allow(clippy::wrong_self_convention)]
    async fn from_core_request(
        &self,
        ctx: &ReqCtx,
        req: &CoreRequest,
        endpoint: &ProviderEndpoint,
    ) -> Result<UpstreamRequest>;
    /// 上游响应 JSON → CoreResponse。
    async fn to_core_response(&self, ctx: &ReqCtx, raw: Value) -> Result<CoreResponse>;
}

/// 上游 SSE → Core 流事件（流式）。
#[async_trait]
pub trait ProviderStreamAdapter: Send + Sync {
    fn protocol(&self) -> Protocol;
    /// 把一个上游 SSE chunk 解码为 0..n 个 Core 流事件。
    ///
    /// gateway 已在解码前对该 chunk 触发 `on_upstream_chunk_raw` 钩子。
    fn decode(&self, ctx: &ReqCtx, chunk: &RawChunk) -> Result<Vec<CoreStreamEvent>>;
}
