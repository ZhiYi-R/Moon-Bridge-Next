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
    /// 命中的 routes 表别名（仅经别名映射解析的请求有值；裸模型名/限定名为 None）。
    /// 插件 route 维度 binding 的 scope_key 依据。
    pub route_alias: Option<String>,
    /// 上游实际模型名，路由解析后填充（插件 model 维度 binding 的 scope_key 依据）。
    pub upstream_model: Option<String>,
    /// 上游模型的输出 token 上限（models 表 `max_output_tokens`，dispatch 随路由
    /// 解析回填）。**不是**客户端设的 `CoreRequest::max_tokens`——仅供
    /// 「max_tokens 必填」的上游协议（Anthropic）在客户端未设上限时兜底取值；
    /// 其余协议客户端没给就依旧不发送该字段，不凭空注入。
    pub upstream_max_output_tokens: Option<u32>,
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
            route_alias: None,
            upstream_model: None,
            upstream_max_output_tokens: None,
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
