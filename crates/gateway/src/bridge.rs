//! [`HostBridge`] 的网关实现：把插件的受控宿主调用落到真实网络/路由。
//!
//! - `http_request`：经共享 reqwest 客户端发起（egress 代理已在客户端构建时施加）。
//! - `provider_invoke`：解析 provider 端点并用对应 Adapter 直接转换调用；
//!   **不触发插件钩子**，避免「插件 → provider_invoke → 插件」的无限递归。

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use moonbridge_core::{CoreRequest, CoreResponse, Protocol};
use moonbridge_plugin::{HostBridge, HttpRequest, HttpResponse};
use moonbridge_protocol::{Registry, ReqCtx};
use moonbridge_store::Database;

use crate::router;
use crate::upstream::send;

/// 插件 http 子请求未显式指定超时时的兜底（防止 raw 钩子内因无超时而挂死）。
const DEFAULT_PLUGIN_HTTP_TIMEOUT: Duration = Duration::from_secs(30);

/// 网关注入给插件运行时的宿主桥。
pub struct GatewayBridge {
    client: reqwest::Client,
    db: Arc<Database>,
    registry: Arc<Registry>,
    /// `provider_invoke` 单次端点调用的总超时。必须给：插件在 Lua 协程里
    /// yield 等待时其 instruction/timeout 预算钩子不会触发——上游挂起若无
    /// 传输层超时，会一直占着该插件的 Lua Mutex，毒化其全部后续调用。
    invoke_timeout: Duration,
}

impl GatewayBridge {
    /// 构造宿主桥。`invoke_timeout` 建议取插件 `call_timeout` 量级。
    pub fn new(
        client: reqwest::Client,
        db: Arc<Database>,
        registry: Arc<Registry>,
        invoke_timeout: Duration,
    ) -> Self {
        GatewayBridge {
            client,
            db,
            registry,
            invoke_timeout,
        }
    }
}

#[async_trait]
impl HostBridge for GatewayBridge {
    async fn http_request(&self, req: HttpRequest) -> Result<HttpResponse, String> {
        let method = reqwest::Method::from_bytes(req.method.to_uppercase().as_bytes())
            .map_err(|e| e.to_string())?;
        let mut builder = self.client.request(method, &req.url);
        for (k, v) in &req.headers {
            builder = builder.header(k.as_str(), v.as_str());
        }
        if let Some(body) = &req.body {
            builder = builder.json(body);
        }
        // 总是施加超时：插件指定则用其值，否则用兜底默认，避免 raw 钩子挂死
        let timeout_ms = req
            .timeout_ms
            .unwrap_or(DEFAULT_PLUGIN_HTTP_TIMEOUT.as_millis() as u64);
        builder = builder.timeout(Duration::from_millis(timeout_ms));
        let resp = builder.send().await.map_err(|e| e.to_string())?;
        let status = resp.status().as_u16();
        let headers: Vec<(String, String)> = resp
            .headers()
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
            .collect();
        let text = resp.text().await.map_err(|e| e.to_string())?;
        let body = serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text));
        Ok(HttpResponse {
            status,
            headers,
            body,
        })
    }

    async fn provider_invoke(
        &self,
        provider: &str,
        model: &str,
        req: CoreRequest,
    ) -> Result<CoreResponse, String> {
        let p = self
            .db
            .get_provider(provider)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("provider '{provider}' 未找到"))?;
        if !p.enabled {
            return Err(format!("provider '{provider}' 已停用"));
        }
        let endpoints = router::endpoints_from_provider(&p).map_err(|e| e.to_string())?;

        let mut req = req;
        req.model = model.to_string();
        // 插件内调用一律非流式：body 按 JSON 一次性解析，stream=true 会让
        // 上游回 SSE 而解析必败
        req.stream = false;

        // 与正常请求路径同口径的逐端点故障转移（429/5xx/传输失败切换），
        // 每次调用施加 invoke_timeout——上游挂起绝不能永久占用插件的
        // Lua Mutex（协程 yield 期间预算钩子不触发）。
        let mut last_err = String::new();
        for ep in &endpoints {
            let Some(adapter) = self.registry.provider(ep.protocol) else {
                last_err = format!("无上游 Adapter 支持协议 {}", ep.protocol);
                continue;
            };
            let mut ctx = ReqCtx::new("plugin-invoke", Protocol::OpenAiResponse)
                .with_route(ep.protocol, provider.to_string());
            ctx.upstream_max_output_tokens = router::model_output_limit(&self.db, model);
            let up = match adapter.from_core_request(&ctx, &req, ep).await {
                Ok(u) => u,
                Err(e) => return Err(e.to_string()),
            };
            let resp = match tokio::time::timeout(self.invoke_timeout, send(&self.client, &up)).await
            {
                Ok(Ok(r)) => r,
                Ok(Err(e)) => {
                    last_err = e.to_string();
                    continue;
                }
                Err(_) => {
                    last_err = format!("上游调用超时（{:?}）", self.invoke_timeout);
                    continue;
                }
            };
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            if status.is_success() {
                let json: serde_json::Value =
                    serde_json::from_str(&text).map_err(|e| e.to_string())?;
                return adapter
                    .to_core_response(&ctx, json)
                    .await
                    .map_err(|e| e.to_string());
            }
            last_err = format!("上游错误[{status}]: {text}");
            // 非重试类 4xx 直接返回，不再尝试后续端点
            if status.as_u16() != 429 && !status.is_server_error() {
                return Err(last_err);
            }
        }
        Err(last_err)
    }
}
