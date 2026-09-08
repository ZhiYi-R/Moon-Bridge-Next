//! 请求上下文：贯穿一次请求生命周期，供 Adapter 与 PluginHooks 读取路由/会话信息。

use moonbridge_core::{Map, Protocol};

/// 一次请求的上下文快照。可克隆，随请求在 dispatch 链路中传递。
#[derive(Debug, Clone)]
pub struct ReqCtx {
    /// 网关分配的请求 ID（用于日志/trace 关联）。
    pub request_id: String,
    /// 会话 ID（来自 `session_id` 字段或 `X-Codex-Window-Id` 头）。
    pub session_id: Option<String>,
    /// 客户端请求的模型别名（路由解析前）。
    pub model_alias: String,
    /// 入口协议（客户端 → 网关）。
    pub client_protocol: Protocol,
    /// 上游协议（网关 → provider），路由解析后填充。
    pub upstream_protocol: Option<Protocol>,
    /// 命中的 provider key，路由解析后填充。
    pub provider_key: Option<String>,
    /// 是否流式请求。
    pub stream: bool,
    /// 其它元数据（原始 headers 摘录、客户端 UA 等）。
    pub meta: Map,
}

impl ReqCtx {
    /// 以入口协议构造一个基础上下文。
    pub fn new(request_id: impl Into<String>, client_protocol: Protocol) -> Self {
        ReqCtx {
            request_id: request_id.into(),
            session_id: None,
            model_alias: String::new(),
            client_protocol,
            upstream_protocol: None,
            provider_key: None,
            stream: false,
            meta: Map::new(),
        }
    }

    /// 记录路由解析结果（上游协议 + provider）。
    pub fn with_route(mut self, upstream: Protocol, provider_key: impl Into<String>) -> Self {
        self.upstream_protocol = Some(upstream);
        self.provider_key = Some(provider_key.into());
        self
    }
}
