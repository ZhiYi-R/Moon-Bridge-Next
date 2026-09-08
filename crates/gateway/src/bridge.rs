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
}

impl GatewayBridge {
    /// 构造宿主桥。
    pub fn new(client: reqwest::Client, db: Arc<Database>, registry: Arc<Registry>) -> Self {
        GatewayBridge {
            client,
            db,
            registry,
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
        let endpoints = router::endpoints_from_provider(&p).map_err(|e| e.to_string())?;
        let protocol = endpoints[0].protocol;
        let adapter = self
            .registry
            .provider(protocol)
            .ok_or_else(|| format!("无上游 Adapter 支持协议 {protocol}"))?;

        let mut req = req;
        req.model = model.to_string();
        let ctx = ReqCtx::new("plugin-invoke", Protocol::OpenAiResponse)
            .with_route(protocol, provider.to_string());

        // 插件内主动调用取首个端点（不做故障转移）
        let up = adapter
            .from_core_request(&ctx, &req, &endpoints[0])
            .await
            .map_err(|e| e.to_string())?;
        let resp = send(&self.client, &up).await.map_err(|e| e.to_string())?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("上游错误[{status}]: {text}"));
        }
        let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
        adapter
            .to_core_response(&ctx, json)
            .await
            .map_err(|e| e.to_string())
    }
}
