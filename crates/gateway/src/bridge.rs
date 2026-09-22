//! [`HostBridge`] 的网关实现：把插件的受控宿主调用落到真实网络/路由。
//!
//! - `http_request`：经共享 reqwest 客户端发起（egress 代理已在客户端构建时施加）。
//! - `provider_invoke`：解析 provider 端点并用对应 Adapter 直接转换调用；
//!   **不触发插件钩子**，避免「插件 → provider_invoke → 插件」的无限递归。
//! - `secret_*`：store secrets 表（scope 已由插件侧冠以插件名）。
//! - `oauth_listen_callback/await/close`：共享 [`CallbackRegistry`]（见 `crate::oauth`）。
//! - `fs_read`：白名单已在插件侧校验，此处再做 home 目录包含性复查（纵深防御）。

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use moonbridge_core::{CoreRequest, CoreResponse, Protocol};
use moonbridge_plugin::{CallbackHandle, CallbackSpec, HostBridge, HttpRequest, HttpResponse};
use moonbridge_protocol::{Registry, ReqCtx};
use moonbridge_store::Database;

use crate::oauth::CallbackRegistry;
use crate::router;
use crate::upstream::send;

/// 插件 http 子请求未显式指定超时时的默认值（防止 raw 钩子内因无超时而挂死）。
const DEFAULT_PLUGIN_HTTP_TIMEOUT: Duration = Duration::from_secs(30);

/// 网关注入给插件运行时的宿主桥。
pub struct GatewayBridge {
    client: reqwest::Client,
    /// 插件 `mb.http` 子请求客户端（不跟随重定向；与上游客户端同 egress 代理）。
    http_client: reqwest::Client,
    db: Arc<Database>,
    registry: Arc<Registry>,
    /// 回环回调监听注册表（与 AppState 共享）。
    callbacks: Arc<CallbackRegistry>,
    /// `provider_invoke` 单次端点调用的总超时。必须给：插件在 Lua 协程里
    /// yield 等待时其 instruction/timeout 预算钩子不会触发——上游挂起若无
    /// 传输层超时，会一直占着该插件的 Lua Mutex，毒化其全部后续调用。
    invoke_timeout: Duration,
}

impl GatewayBridge {
    /// 构造宿主桥。`invoke_timeout` 建议取插件 `call_timeout` 量级。
    pub fn new(
        client: reqwest::Client,
        http_client: reqwest::Client,
        db: Arc<Database>,
        registry: Arc<Registry>,
        callbacks: Arc<CallbackRegistry>,
        invoke_timeout: Duration,
    ) -> Self {
        GatewayBridge {
            client,
            http_client,
            db,
            registry,
            callbacks,
            invoke_timeout,
        }
    }
}

#[async_trait]
impl HostBridge for GatewayBridge {
    async fn http_request(&self, req: HttpRequest) -> Result<HttpResponse, String> {
        let method = reqwest::Method::from_bytes(req.method.to_uppercase().as_bytes())
            .map_err(|e| e.to_string())?;
        let mut builder = self.http_client.request(method, &req.url);
        for (k, v) in &req.headers {
            builder = builder.header(k.as_str(), v.as_str());
        }
        if let Some(form) = &req.form {
            builder = builder.form(form);
        } else if let Some(body) = &req.body {
            builder = builder.json(body);
        }
        // 总是施加超时：插件指定则用其值，否则用默认值，避免 raw 钩子挂死
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
            let resp =
                match tokio::time::timeout(self.invoke_timeout, send(&self.client, &up)).await {
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

    async fn secret_get(&self, scope: &str, key: &str) -> Result<Option<String>, String> {
        self.db.secret_get(scope, key).map_err(|e| e.to_string())
    }

    async fn secret_set(&self, scope: &str, key: &str, value: &str) -> Result<(), String> {
        self.db
            .secret_set(scope, key, value)
            .map_err(|e| e.to_string())
    }

    async fn secret_delete(&self, scope: &str, key: &str) -> Result<(), String> {
        self.db.secret_delete(scope, key).map_err(|e| e.to_string())
    }

    fn random_bytes(&self, n: usize) -> Result<Vec<u8>, String> {
        let mut buf = vec![0u8; n];
        rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut buf);
        Ok(buf)
    }

    async fn oauth_listen_callback(&self, spec: CallbackSpec) -> Result<CallbackHandle, String> {
        self.callbacks.listen(spec).await
    }

    async fn oauth_callback_await(
        &self,
        id: &str,
        timeout_ms: u64,
    ) -> Result<Option<serde_json::Value>, String> {
        self.callbacks
            .await_callback(id, Duration::from_millis(timeout_ms))
            .await
    }

    async fn oauth_callback_close(&self, id: &str) -> Result<(), String> {
        self.callbacks.close(id);
        Ok(())
    }

    async fn open_external(&self, url: &str) -> Result<(), String> {
        open_in_browser(url)
    }

    async fn fs_read(&self, path: &str, max_bytes: u64) -> Result<String, String> {
        let path = std::path::PathBuf::from(path);
        // 纵深防御：白名单在插件侧已校验，这里再确认落在 home 目录内
        let home = dirs::home_dir().ok_or("无法定位 home 目录")?;
        if !path.starts_with(&home) {
            return Err(format!("读取路径越出 home 目录: {}", path.display()));
        }
        let meta = std::fs::metadata(&path).map_err(|e| format!("读取文件失败: {e}"))?;
        if meta.len() > max_bytes {
            return Err(format!(
                "文件超过大小上限（{} > {max_bytes} 字节）",
                meta.len()
            ));
        }
        let text = std::fs::read_to_string(&path).map_err(|e| format!("读取文件失败: {e}"))?;
        Ok(text)
    }
}

/// 在系统默认浏览器打开 URL（scheme 已在插件侧限定为 http/https）。
fn open_in_browser(url: &str) -> Result<(), String> {
    #[cfg(windows)]
    let mut cmd = {
        let mut c = std::process::Command::new("cmd");
        c.args(["/c", "start", "", url]);
        c
    };
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = std::process::Command::new("open");
        c.arg(url);
        c
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut cmd = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(url);
        c
    };
    cmd.spawn()
        .map(|_| ())
        .map_err(|e| format!("打开外部浏览器失败: {e}"))
}
