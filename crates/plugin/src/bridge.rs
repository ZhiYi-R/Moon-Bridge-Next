//! 宿主桥接契约：插件通过它访问受控的宿主能力（HTTP 子请求、跨 provider 调用）。
//!
//! 定义在 plugin crate，由 gateway 实现并注入——这样 plugin 不依赖 gateway，
//! 维持单向依赖。所有网络访问都经此集中，便于统一施加 egress proxy 与默认超时。
//! 注意：集中 ≠ 授权——目前**没有**目标地址/域名白名单，插件子请求可达任意主机
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
    /// 请求体（application/x-www-form-urlencoded）；与 `body` 互斥，给出时优先——
    /// OAuth 端点（RFC 8628/6749）只收表单。
    #[serde(default)]
    pub form: Option<Vec<(String, String)>>,
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

/// OAuth 回环回调监听请求（插件 → 宿主）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallbackSpec {
    /// 监听路径（默认 `/callback`）。
    #[serde(default = "default_callback_path")]
    pub path: String,
    /// CORS 允许源（浏览器页面内 fetch 回调时按源钉死；空数组不发 CORS 头）。
    #[serde(default)]
    pub origins: Vec<String>,
    /// 首选端口（被占用时由宿主退随机端口；`None` 直接随机）。
    #[serde(default)]
    pub preferred_port: Option<u16>,
}

fn default_callback_path() -> String {
    "/callback".to_string()
}

/// 回调监听句柄（宿主 → 插件）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallbackHandle {
    /// 监听 id（后续 await/close 凭它定位）。
    pub id: String,
    /// 实际绑定端口。
    pub port: u16,
    /// 完整回调 URL（`http://127.0.0.1:{port}{path}`）。
    pub url: String,
}

/// 宿主桥接。gateway 实现该 trait 并注入插件运行时。
#[async_trait]
pub trait HostBridge: Send + Sync {
    /// 执行 HTTP 子请求。实现方负责 egress proxy 与默认超时；目标地址/域名过滤
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

    // ── 以下能力默认拒绝：旁路窄桥不实现时插件得到明确错误 ──

    /// 读加密小值（scope 已在宿主侧按插件命名空间隔离）。
    async fn secret_get(&self, _scope: &str, _key: &str) -> Result<Option<String>, String> {
        Err("宿主不支持 mb.secret".to_string())
    }
    /// 写加密小值。
    async fn secret_set(&self, _scope: &str, _key: &str, _value: &str) -> Result<(), String> {
        Err("宿主不支持 mb.secret".to_string())
    }
    /// 删加密小值。
    async fn secret_delete(&self, _scope: &str, _key: &str) -> Result<(), String> {
        Err("宿主不支持 mb.secret".to_string())
    }
    /// CSPRNG 随机字节（沙箱无安全随机源，state/verifier 必须从这里取）。
    fn random_bytes(&self, _n: usize) -> Result<Vec<u8>, String> {
        Err("宿主不支持 mb.random".to_string())
    }
    /// 启动一次性 OAuth 回环回调监听（state 校验与清理由宿主负责）。
    async fn oauth_listen_callback(&self, _spec: CallbackSpec) -> Result<CallbackHandle, String> {
        Err("宿主不支持 mb.oauth.listen_callback".to_string())
    }
    /// 等待回调参数到达；超时返回 `Ok(None)`。监听 id 无效返回 Err。
    async fn oauth_callback_await(
        &self,
        _id: &str,
        _timeout_ms: u64,
    ) -> Result<Option<Value>, String> {
        Err("宿主不支持 mb.oauth.callback_await".to_string())
    }
    /// 关闭回调监听（幂等；完成/超时/取消都应调用）。
    async fn oauth_callback_close(&self, _id: &str) -> Result<(), String> {
        Err("宿主不支持 mb.oauth.callback_close".to_string())
    }
    /// 在外部浏览器打开 URL。
    async fn open_external(&self, _url: &str) -> Result<(), String> {
        Err("宿主不支持 mb.open_external".to_string())
    }
    /// 受限文件读（白名单已在插件侧校验；宿主只做读取与大小截断）。
    async fn fs_read(&self, _path: &str, _max_bytes: u64) -> Result<String, String> {
        Err("宿主不支持 mb.fs.read".to_string())
    }
}
