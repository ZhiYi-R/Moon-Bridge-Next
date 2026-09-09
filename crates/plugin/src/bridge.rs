//! 宿主桥接契约：插件通过它访问受控的宿主能力（HTTP 子请求、跨 provider 调用）。
//!
//! 定义在 plugin crate，由 gateway 实现并注入——这样 plugin 不依赖 gateway，
//! 维持单向依赖。所有网络访问都经此收口，便于统一施加 egress proxy 与兜底超时。
//! 注意：收口 ≠ 授权——目前**没有**目标地址/域名白名单，插件子请求可达任意主机
//! （含内网与链路本地地址），需要限制时应在 gateway 侧的实现里补。

use async_trait::async_trait;
use moonbridge_core::{CoreRequest, CoreResponse};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 插件发起的受控 HTTP 子请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpRequest {
    /// HTTP 方法（GET/POST/...），默认 GET。
    #[serde(default = "default_method")]
    pub method: String,
    pub url: String,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    /// 请求体（JSON）；`None` 表示无体。
    #[serde(default)]
    pub body: Option<Value>,
    /// 超时（毫秒）；`None` 用宿主默认。
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

fn default_method() -> String {
    "GET".to_string()
}

/// 受控 HTTP 子请求的响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpResponse {
    pub status: u16,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    /// 响应体：优先解析为 JSON，否则以字符串承载于 `Value::String`。
    #[serde(default)]
    pub body: Value,
}

/// 宿主桥接。gateway 实现该 trait 并注入插件运行时。
#[async_trait]
pub trait HostBridge: Send + Sync {
    /// 执行 HTTP 子请求。实现方负责 egress proxy 与兜底超时；目标地址/域名过滤
    /// 属实现方的应有职责，但当前 gateway 实现**尚未**提供（见模块文档）。
    async fn http_request(&self, req: HttpRequest) -> Result<HttpResponse, String>;

    /// 跨 provider 调用：以 Core IR 直接请求另一个 provider（用于 visual/websearch
    /// 式编排插件）。实现方复用网关的路由与协议转换链路。
    async fn provider_invoke(
        &self,
        provider: &str,
        model: &str,
        req: CoreRequest,
    ) -> Result<CoreResponse, String>;
}
