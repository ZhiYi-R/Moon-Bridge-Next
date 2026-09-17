
//! OAuth 令牌包与登录/刷新机制（Command Code / Kimi）。
//!
//! 设计要点：
//! - **零 schema 变更**：令牌包存于 provider `extra_json` 的 `oauth` 子对象，当前 access
//!   token 同步写入端点 `api_key`——现有出站链路（Bearer、故障转移、计价、trace）不感知
//!   OAuth 的存在。
//! - **Kimi**：RFC 8628 设备码授权（`auth.kimi.com`），令牌有有效期；`ensure_fresh` 在
//!   dispatch 出站前检查并刷新（提前 5 分钟视为过期），刷新回写后由 dispatch 重新解析路由。
//! - **Command Code**：登录产物是长期 API key（无刷新端点），`expires_at = i64::MAX`
//!   使 `needs_refresh` 恒 false，刷新链路自然不触发。
//!
//! 流程参数对齐 opencodex `src/oauth/{kimi,command-code}.ts` 的实测契约。

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use moonbridge_store::{Database, Provider};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::state::AppState;

/// Kimi Code 的公开 OAuth client id（与 KimiCLI 共用）。
pub const KIMI_CLIENT_ID: &str = "17e5f671-d194-4dfb-9706-5516cb48c098";
/// Kimi OAuth 主机。
pub const KIMI_OAUTH_HOST: &str = "https://auth.kimi.com";
/// 随请求上报的 KimiCLI 版本号（服务端按此识别客户端形态）。
const KIMI_CLI_VERSION: &str = "0.14.0";
/// Command Code Studio 站点（登录页与回调 CORS 源）。
pub const COMMAND_CODE_STUDIO: &str = "https://commandcode.ai";
/// Command Code 凭据验证端点。
const COMMAND_CODE_WHOAMI: &str = "https://api.commandcode.ai/alpha/whoami";
/// 回调首选端口（被占用时退随机端口）。
pub const COMMAND_CODE_CALLBACK_PORT: u16 = 5959;
/// 令牌有效期安全余量：提前 5 分钟视为过期（对齐 opencodex OAUTH_EXPIRY_SKEW_MS）。
pub const EXPIRY_SKEW_MS: i64 = 5 * 60 * 1000;

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ───────────────────────── 令牌包 ─────────────────────────

/// OAuth 令牌包（provider `extra_json.oauth`；camelCase 与前端 DTO 一致）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct OAuthBundle {
    /// 登录机制：`kimi`（设备码，可刷新）/ `command-code`（长期 key，不刷新）。
    pub kind: String,
    pub access: String,
    pub refresh: String,
    /// 过期时刻（unix ms）；长期 key 记 `i64::MAX`（`needs_refresh` 恒 false）。
    pub expires_at: i64,
    #[serde(default)]
    pub account_id: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    /// 来源：oauth / local-cli / manual。
    #[serde(default)]
    pub source: Option<String>,
    /// Kimi 设备 id（登录时生成的稳定值，刷新时沿用同一份 `X-Msh-Device-Id`）。
    #[serde(default)]
    pub device_id: Option<String>,
}

impl OAuthBundle {
    /// 长期 key 的令牌包（Command Code：无刷新端点，refresh 与 access 同值）。
    pub fn durable(kind: &str, access: String, account_id: Option<String>, source: &str) -> Self {
        Self {
            kind: kind.to_string(),
            refresh: access.clone(),
            access,
            expires_at: i64::MAX,
            account_id,
            email: None,
            source: Some(source.to_string()),
            device_id: None,
        }
    }
}

/// 从 provider.extra 解析 OAuth 令牌包。extra 用户可编辑，解析失败不当致命错误。
pub fn bundle_of(p: &Provider) -> Option<OAuthBundle> {
    serde_json::from_value(p.extra.get("oauth")?.clone()).ok()
}

/// 是否临期（长期 key 恒 false；提前 5 分钟即视为过期）。
pub fn needs_refresh(b: &OAuthBundle, now: i64) -> bool {
    b.expires_at != i64::MAX && b.expires_at - EXPIRY_SKEW_MS <= now
}

/// 快速判定 provider 是否临期（dispatch 每请求调用；读库失败/无令牌包均 false）。
pub fn provider_needs_refresh(db: &Database, provider_key: &str) -> bool {
    match db.get_provider(provider_key) {
        Ok(Some(p)) => bundle_of(&p).is_some_and(|b| needs_refresh(&b, now_ms())),
        _ => false,
    }
}

/// 出站前保鲜：临期则刷新并回写（端点 api_key + extra.oauth），返回是否发生了刷新
/// （true 时调用方应重新解析路由以拿到新凭据）。
///
/// 串行化由 `AppState::oauth_refresh_lock` 保证：并发请求不双刷同一 refresh_token
/// （服务端若轮换 refresh，双刷会互踢失效）。
pub async fn ensure_fresh(state: &AppState, provider_key: &str) -> Result<bool, String> {
    let _guard = state.oauth_refresh_lock.lock().await;
    let p = match state.db.get_provider(provider_key) {
        Ok(Some(p)) => p,
        _ => return Ok(false),
    };
    let Some(bundle) = bundle_of(&p) else {
        return Ok(false);
    };
    // 双检：等锁期间可能已被另一请求刷新
    if !needs_refresh(&bundle, now_ms()) {
        return Ok(false);
    }
    match bundle.kind.as_str() {
        "kimi" => {
            let fresh = kimi_refresh(&state.client, &bundle).await?;
            write_bundle(&state.db, p, &fresh)?;
            tracing::info!(provider = %provider_key, "OAuth 令牌已刷新（kimi）");
            Ok(true)
        }
        // command-code 为长期 key（永不临期）；未知 kind 不刷新
        _ => Ok(false),
    }
}

/// 把新令牌包回写 provider：端点 api_key 同步为最新 access token。
/// OAuth provider 按登录构造只有一个端点；多端点情形只更新第一个（主端点）。
fn write_bundle(db: &Database, mut p: Provider, bundle: &OAuthBundle) -> Result<(), String> {
    if let Some(ep) = p.endpoints.first_mut() {
        ep.api_key = bundle.access.clone();
    }
    let mut extra = p.extra.as_object().cloned().unwrap_or_default();
    extra.insert(
        "oauth".to_string(),
        serde_json::to_value(bundle).map_err(|e| e.to_string())?,
    );
    p.extra = Value::Object(extra);
    db.upsert_provider(&p).map_err(|e| e.to_string())
}

// ───────────────────────── Kimi：RFC 8628 设备码 ─────────────────────────

/// Kimi 设备码授权响应（已展开为内部形态）。
#[derive(Debug, Clone)]
pub struct DeviceAuth {
    pub verification_url: String,
    pub user_code: String,
    pub device_code: String,
    pub expires_in_ms: u64,
    pub interval_ms: u64,
}

/// KimiCLI 身份头（服务端按此识别客户端形态；device id 为登录安装级稳定值）。
fn kimi_headers(device_id: &str) -> reqwest::header::HeaderMap {
    use reqwest::header::{HeaderMap, HeaderValue};
    let mut h = HeaderMap::new();
    let ua = format!("KimiCLI/{KIMI_CLI_VERSION}");
    if let Ok(v) = HeaderValue::from_str(&ua) {
        h.insert(reqwest::header::USER_AGENT, v);
    }
    h.insert("X-Msh-Platform", HeaderValue::from_static("kimi_code_cli"));
    if let Ok(v) = HeaderValue::from_str(KIMI_CLI_VERSION) {
        h.insert("X-Msh-Version", v);
    }
    let hostname = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "unknown".to_string());
    let os_model = format!("{} {}", std::env::consts::OS, std::env::consts::ARCH);
    for (k, v) in [
        ("X-Msh-Device-Name", hostname),
        ("X-Msh-Device-Model", os_model),
        ("X-Msh-Os-Version", std::env::consts::OS.to_string()),
        ("X-Msh-Device-Id", device_id.to_string()),
    ] {
        if let Ok(v) = HeaderValue::from_str(&v) {
            h.insert(k, v);
        }
    }
    h
}

/// 请求设备授权码。
pub async fn kimi_device_authorize(
    client: &reqwest::Client,
    device_id: &str,
) -> Result<DeviceAuth, String> {
    let resp = client
        .post(format!("{KIMI_OAUTH_HOST}/api/oauth/device_authorization"))
        .headers(kimi_headers(device_id))
        .form(&[("client_id", KIMI_CLIENT_ID)])
        .send()
        .await
        .map_err(|e| format!("Kimi 设备授权请求失败: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("Kimi 设备授权返回 {}", resp.status()));
    }
    let v: Value = resp
        .json()
        .await
        .map_err(|e| format!("Kimi 设备授权响应不是合法 JSON: {e}"))?;
    let get_str = |k: &str| v.get(k).and_then(Value::as_str).filter(|s| !s.is_empty());
    let (Some(user_code), Some(device_code)) = (get_str("user_code"), get_str("device_code"))
    else {
        return Err("Kimi 设备授权响应缺少 user_code/device_code".to_string());
    };
    let Some(verification) = get_str("verification_uri_complete").or(get_str("verification_uri"))
    else {
        return Err("Kimi 设备授权响应缺少 verification_uri".to_string());
    };
    let num = |k: &str| v.get(k).and_then(Value::as_f64).filter(|n| n.is_finite() && *n > 0.0);
    Ok(DeviceAuth {
        verification_url: verification.to_string(),
        user_code: user_code.to_string(),
        device_code: device_code.to_string(),
        expires_in_ms: num("expires_in").map(|s| (s * 1000.0) as u64).unwrap_or(15 * 60 * 1000),
        interval_ms: num("interval").map(|s| (s * 1000.0) as u64).unwrap_or(5000),
    })
}

/// 解码 JWT payload（不验签——只取身份声明，令牌有效性由服务端裁决）。
fn jwt_payload(token: &str) -> Option<Value> {
    let mut parts = token.split('.');
    let _header = parts.next()?;
    let payload = parts.next()?;
    let _signature = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// 从 access/refresh 两个 JWT 提取身份：user_id 优先于 sub，email 取先出现者并小写化。
fn kimi_identity(access: &str, refresh: Option<&str>) -> (Option<String>, Option<String>) {
    let a = jwt_payload(access);
    let r = refresh.and_then(jwt_payload);
    let pick = |k: &str| {
        a.as_ref()
            .and_then(|v| v.get(k))
            .or_else(|| r.as_ref().and_then(|v| v.get(k)))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let account = pick("user_id").or_else(|| pick("sub"));
    let email = pick("email").map(|e| e.to_lowercase());
    (account, email)
}

/// 解析 Kimi token 响应。expires_in 必须是有限非负数（NaN/负值会产生永不刷新或
/// 立即过期的令牌）；refresh 缺失时回退调用方持有的旧 refresh token。
fn parse_kimi_token(
    v: &Value,
    refresh_fallback: Option<&str>,
    device_id: &str,
) -> Result<OAuthBundle, String> {
    let access = v
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or("Kimi token 响应缺少 access_token")?;
    let expires_in = v
        .get("expires_in")
        .and_then(Value::as_f64)
        .filter(|n| n.is_finite() && *n >= 0.0)
        .ok_or("Kimi token 响应缺少合法 expires_in")?;
    let expires = now_ms()
        .checked_add((expires_in * 1000.0) as i64)
        .and_then(|t| t.checked_sub(EXPIRY_SKEW_MS))
        .ok_or("Kimi token 过期时间溢出")?;
    let refresh = v
        .get("refresh_token")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| refresh_fallback.map(str::to_string))
        .ok_or("Kimi token 响应缺少 refresh_token")?;
    let (account_id, email) = kimi_identity(access, Some(&refresh));
    Ok(OAuthBundle {
        kind: "kimi".to_string(),
        access: access.to_string(),
        refresh,
        expires_at: expires,
        account_id,
        email,
        source: Some("oauth".to_string()),
        device_id: Some(device_id.to_string()),
    })
}

/// 轮询换取令牌（authorization_pending / slow_down 语义按 RFC 8628）；可中止。
pub async fn kimi_poll_token(
    client: &reqwest::Client,
    device_id: &str,
    auth: &DeviceAuth,
    abort: &mut tokio::sync::oneshot::Receiver<()>,
) -> Result<OAuthBundle, String> {
    let deadline = now_ms() + auth.expires_in_ms as i64;
    let mut wait_ms = auth.interval_ms.max(1000);
    loop {
        if now_ms() >= deadline {
            return Err("Kimi 设备授权超时，请重新发起登录".to_string());
        }
        let resp = client
            .post(format!("{KIMI_OAUTH_HOST}/api/oauth/token"))
            .headers(kimi_headers(device_id))
            .form(&[
                ("client_id", KIMI_CLIENT_ID),
                ("device_code", auth.device_code.as_str()),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ])
            .send()
            .await
            .map_err(|e| format!("Kimi token 轮询失败: {e}"))?;
        let status = resp.status();
        let v: Value = resp
            .json()
            .await
            .map_err(|e| format!("Kimi token 响应不是合法 JSON: {e}"))?;
        if status.is_success() {
            return parse_kimi_token(&v, None, device_id);
        }
        match v.get("error").and_then(Value::as_str) {
            Some("authorization_pending") => {}
            Some("slow_down") => {
                wait_ms += 5000;
                if let Some(iv) = v.get("interval").and_then(Value::as_f64) {
                    let iv = (iv * 1000.0) as u64;
                    if iv > wait_ms {
                        wait_ms = iv;
                    }
                }
            }
            Some("expired_token") => return Err("Kimi 设备授权已过期".to_string()),
            Some("access_denied") => return Err("Kimi 设备授权被拒绝".to_string()),
            Some(other) => {
                let desc = v.get("error_description").and_then(Value::as_str).unwrap_or("");
                return Err(format!("Kimi 授权失败: {other} {desc}"));
            }
            None => return Err(format!("Kimi token 轮询返回 {status}")),
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(wait_ms)) => {}
            _ = &mut *abort => return Err("登录已取消".to_string()),
        }
    }
}

/// 刷新 Kimi 令牌（refresh token 失效时返回错误，调用侧提示重新登录）。
pub async fn kimi_refresh(
    client: &reqwest::Client,
    bundle: &OAuthBundle,
) -> Result<OAuthBundle, String> {
    let device_id = bundle.device_id.as_deref().unwrap_or("");
    let resp = client
        .post(format!("{KIMI_OAUTH_HOST}/api/oauth/token"))
        .headers(kimi_headers(device_id))
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", bundle.refresh.as_str()),
            ("client_id", KIMI_CLIENT_ID),
        ])
        .send()
        .await
        .map_err(|e| format!("Kimi 令牌刷新失败: {e}"))?;
    let status = resp.status();
    let v: Value = resp
        .json()
        .await
        .map_err(|e| format!("Kimi 刷新响应不是合法 JSON: {e}"))?;
    if !status.is_success() {
        let desc = v.get("error_description").and_then(Value::as_str).unwrap_or("");
        return Err(format!("Kimi 令牌刷新被拒绝（{status}），请重新登录: {desc}"));
    }
    parse_kimi_token(&v, Some(&bundle.refresh), device_id)
}

// ───────────────────────── Command Code：回调 + 本地导入 + 粘贴 ─────────────────────────

/// 回调负载（Studio 页面 POST 到本地回调的 JSON；camelCase 是线格式）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandCodeCallback {
    pub api_key: String,
    pub state: String,
    #[serde(default)]
    pub user_id: String,
    #[serde(default)]
    pub user_name: String,
    #[serde(default)]
    pub key_name: String,
}

/// 一次性回调监听器：收到合法回调后经 oneshot 交出，`stop` 优雅关闭服务器。
pub struct CallbackListener {
    pub port: u16,
    pub state: String,
    pub rx: tokio::sync::oneshot::Receiver<CommandCodeCallback>,
    shutdown: tokio::sync::oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}

impl CallbackListener {
    /// 优雅关闭（句柄随调用消耗）。
    pub fn stop(self) {
        let _ = self.shutdown.send(());
        self.task.abort();
    }
}

/// 启动回调监听：首选 5959，占用则随机端口；仅接受 state 匹配的 POST /callback。
///
/// CORS 钉死 COMMAND_CODE_STUDIO 源（回调由该站点的页面内 fetch 发起，浏览器强制预检）。
pub async fn start_command_code_listener() -> Result<CallbackListener, String> {
    use axum::{extract::State as AxState, routing::post, Json, Router};
    use tower_http::cors::CorsLayer;

    let state = uuid::Uuid::new_v4().simple().to_string();
    let (tx, rx) = tokio::sync::oneshot::channel::<CommandCodeCallback>();
    let (sd_tx, sd_rx) = tokio::sync::oneshot::channel::<()>();
    // 回调只送达一次：tx 取走即完成
    let shared = std::sync::Arc::new(std::sync::Mutex::new(Some(tx)));
    let expected = state.clone();

    let app = Router::new()
        .route(
            "/callback",
            post(
                move |AxState(shared): AxState<std::sync::Arc<std::sync::Mutex<Option<tokio::sync::oneshot::Sender<CommandCodeCallback>>>>>,
                      Json(body): Json<Value>| async move {
                    let parsed: Result<CommandCodeCallback, _> = serde_json::from_value(body);
                    match parsed {
                        Ok(cb) if cb.state == expected && !cb.api_key.is_empty() => {
                            let tx = shared.lock().unwrap().take();
                            if let Some(tx) = tx {
                                let _ = tx.send(cb);
                            }
                            Json(serde_json::json!({ "success": true }))
                        }
                        Ok(_) => Json(serde_json::json!({ "success": false, "error": "state mismatch" })),
                        Err(e) => Json(serde_json::json!({ "success": false, "error": e.to_string() })),
                    }
                },
            ),
        )
        .with_state(shared)
        .layer(
            CorsLayer::new()
                .allow_origin(tower_http::cors::AllowOrigin::exact(http::HeaderValue::from_static(
                    COMMAND_CODE_STUDIO,
                )))
                .allow_methods([http::Method::POST, http::Method::OPTIONS])
                .allow_headers([http::header::CONTENT_TYPE]),
        );

    let listener = match tokio::net::TcpListener::bind(("127.0.0.1", COMMAND_CODE_CALLBACK_PORT)).await {
        Ok(l) => l,
        Err(_) => tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .map_err(|e| format!("回调端口绑定失败: {e}"))?,
    };
    let port = listener
        .local_addr()
        .map_err(|e| format!("读取回调端口失败: {e}"))?
        .port();
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = sd_rx.await;
            })
            .await;
    });
    Ok(CallbackListener {
        port,
        state,
        rx,
        shutdown: sd_tx,
        task,
    })
}

/// Studio 登录页 URL（携带回调地址与 state）。
pub fn command_code_auth_url(port: u16, state: &str) -> String {
    let callback = format!("http://127.0.0.1:{port}/callback");
    let qs = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("callback", &callback)
        .append_pair("state", state)
        .finish();
    format!("{COMMAND_CODE_STUDIO}/studio/auth/cli?{qs}")
}

/// whoami 验证：合法 key 返回 (userId, userName)；非法/网络失败均 None。
pub async fn command_code_whoami(client: &reqwest::Client, api_key: &str) -> Option<(String, String)> {
    let resp = client
        .get(COMMAND_CODE_WHOAMI)
        .bearer_auth(api_key)
        .header(reqwest::header::ACCEPT, "application/json")
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let v: Value = resp.json().await.ok()?;
    let user = v.get("user")?;
    let id = user.get("id")?.as_str()?.to_string();
    let name = user.get("userName")?.as_str()?.to_string();
    if id.is_empty() || name.is_empty() {
        return None;
    }
    Some((id, name))
}

/// 导入本地 CLI 凭据（~/.commandcode/auth.json，whoami 在线验证）。
/// whoami 失败时回退文件内的 userId（凭据仍接受——文件存在即 CLI 已登录过）。
pub async fn command_code_import_local(client: &reqwest::Client) -> Option<OAuthBundle> {
    let home = dirs::home_dir()?;
    let text = std::fs::read_to_string(home.join(".commandcode").join("auth.json")).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    let key = v.get("apiKey")?.as_str()?.to_string();
    if key.is_empty() {
        return None;
    }
    let account = match command_code_whoami(client, &key).await {
        Some((id, _)) => Some(id),
        None => v
            .get("userId")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
    };
    Some(OAuthBundle::durable("command-code", key, account, "local-cli"))
}

/// 手动粘贴的解析结果。
#[derive(Debug, Clone)]
pub enum CommandCodePaste {
    /// 回调 JSON / URL（state 已校验匹配）。
    Callback(CommandCodeCallback),
    /// 裸 API key（无 state 可比对，仅形态校验；合法性由 whoami 兜底）。
    RawKey(String),
}

/// 解析手动粘贴：回调 JSON / 含 apiKey 的 URL / 裸 key。
/// URL 形态必须带匹配 state（防止旧会话或他人回调被接收）；裸 key 豁免。
pub fn parse_command_code_paste(
    input: &str,
    expected_state: &str,
) -> Result<CommandCodePaste, String> {
    let t = input.trim();
    if t.is_empty() {
        return Err("粘贴内容为空".to_string());
    }
    if t.starts_with('{') {
        let cb: CommandCodeCallback =
            serde_json::from_str(t).map_err(|e| format!("回调 JSON 解析失败: {e}"))?;
        if cb.state != expected_state {
            return Err("state 不匹配（可能是旧会话的回调）".to_string());
        }
        if cb.api_key.is_empty() {
            return Err("回调缺少 apiKey".to_string());
        }
        return Ok(CommandCodePaste::Callback(cb));
    }
    if t.starts_with("http://") || t.starts_with("https://") {
        let u = url::Url::parse(t).map_err(|e| format!("URL 解析失败: {e}"))?;
        let mut key: Option<String> = None;
        let mut state: Option<String> = None;
        // query 与 fragment 都查（页面跳转可能把参数放在 hash 里）
        let frag_pairs: Vec<(String, String)> = u
            .fragment()
            .map(|f| url::form_urlencoded::parse(f.as_bytes()).into_owned().collect())
            .unwrap_or_default();
        for (k, v) in u.query_pairs().into_owned().chain(frag_pairs) {
            match k.as_str() {
                "apiKey" | "api_key" | "key" | "token" => key = Some(v),
                "state" => state = Some(v),
                _ => {}
            }
        }
        let key = key.ok_or("URL 中未找到 apiKey")?;
        match state.as_deref() {
            Some(s) if s == expected_state => {}
            _ => return Err("state 不匹配（可能是旧会话的回调）".to_string()),
        }
        return Ok(CommandCodePaste::Callback(CommandCodeCallback {
            api_key: key,
            state: expected_state.to_string(),
            user_id: String::new(),
            user_name: String::new(),
            key_name: "manual".to_string(),
        }));
    }
    if t.chars().any(char::is_whitespace) {
        return Err("不是合法的 API key（含空白字符）".to_string());
    }
    Ok(CommandCodePaste::RawKey(t.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn kimi_bundle(expires_at: i64) -> OAuthBundle {
        OAuthBundle {
            kind: "kimi".into(),
            access: "a".into(),
            refresh: "r".into(),
            expires_at,
            account_id: None,
            email: None,
            source: Some("oauth".into()),
            device_id: Some("dev".into()),
        }
    }

    #[test]
    fn durable_bundle_never_needs_refresh() {
        let b = OAuthBundle::durable("command-code", "k".into(), None, "oauth");
        assert!(!needs_refresh(&b, i64::MAX - 1));
        assert_eq!(b.refresh, b.access);
    }

    #[test]
    fn needs_refresh_respects_skew() {
        let now = 1_000_000_000_000i64;
        assert!(needs_refresh(&kimi_bundle(now + EXPIRY_SKEW_MS), now), "余量边界即过期");
        assert!(!needs_refresh(&kimi_bundle(now + EXPIRY_SKEW_MS + 1), now));
        assert!(needs_refresh(&kimi_bundle(now - 1), now));
    }

    #[test]
    fn bundle_roundtrip_via_extra_and_invalid_is_none() {
        let mut p = Provider {
            key: "k".into(),
            endpoints: vec![],
            version: None,
            user_agent: None,
            web_search: None,
            extra: json!({ "oauth": { "kind": "kimi", "access": "a", "refresh": "r", "expiresAt": 123 } }),
            enabled: true,
            created_at: 0,
            updated_at: 0,
        };
        let b = bundle_of(&p).expect("应解析出令牌包");
        assert_eq!(b.expires_at, 123);
        assert_eq!(b.device_id, None, "缺省字段为 None");
        p.extra = json!({ "oauth": { "kind": 42 } });
        assert!(bundle_of(&p).is_none(), "非法包不致命，返回 None");
        p.extra = Value::Null;
        assert!(bundle_of(&p).is_none());
    }

    #[test]
    fn provider_needs_refresh_with_db() {
        let db = Database::open_in_memory().unwrap();
        assert!(!provider_needs_refresh(&db, "absent"));
        let mut p = Provider {
            key: "k".into(),
            endpoints: vec![moonbridge_store::Endpoint {
                protocol: "openai-chat".into(),
                base_url: "https://x".into(),
                api_key: "old".into(),
            }],
            version: None,
            user_agent: None,
            web_search: None,
            extra: Value::Null,
            enabled: true,
            created_at: 0,
            updated_at: 0,
        };
        db.upsert_provider(&p).unwrap();
        assert!(!provider_needs_refresh(&db, "k"), "无令牌包不刷新");
        p.extra = json!({ "oauth": kimi_bundle(now_ms() - 1000) });
        db.upsert_provider(&p).unwrap();
        assert!(provider_needs_refresh(&db, "k"), "过期令牌应判定为待刷新");
    }

    #[test]
    fn write_bundle_syncs_endpoint_key_and_extra() {
        let db = Database::open_in_memory().unwrap();
        let p = Provider {
            key: "k".into(),
            endpoints: vec![moonbridge_store::Endpoint {
                protocol: "openai-chat".into(),
                base_url: "https://x".into(),
                api_key: "old-token".into(),
            }],
            version: None,
            user_agent: None,
            web_search: None,
            extra: Value::Null,
            enabled: true,
            created_at: 0,
            updated_at: 0,
        };
        db.upsert_provider(&p).unwrap();
        let fresh = kimi_bundle(now_ms() + 3_600_000);
        write_bundle(&db, p, &OAuthBundle { access: "new-token".into(), ..fresh.clone() }).unwrap();
        let got = db.get_provider("k").unwrap().unwrap();
        assert_eq!(got.endpoints[0].api_key, "new-token", "端点 key 同步为最新 access");
        assert_eq!(bundle_of(&got).unwrap().expires_at, fresh.expires_at);
    }

    // 构造不验签的 JWT（header.payload.sig）
    fn fake_jwt(payload: Value) -> String {
        let enc = |v: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v);
        format!(
            "{}.{}.sig",
            enc(br#"{"alg":"none"}"#),
            enc(payload.to_string().as_bytes())
        )
    }

    #[test]
    fn jwt_payload_decodes_three_part_only() {
        let t = fake_jwt(json!({ "user_id": "u1" }));
        assert_eq!(jwt_payload(&t).unwrap().get("user_id").unwrap(), "u1");
        assert!(jwt_payload("a.b").is_none(), "两段式不解析");
        assert!(jwt_payload("a.&&&.c").is_none(), "非法 base64 不解析");
    }

    #[test]
    fn parse_kimi_token_happy_and_fallback() {
        let access = fake_jwt(json!({ "user_id": "u1", "email": "A@B.COM" }));
        let refresh = fake_jwt(json!({ "user_id": "u9" }));
        let v = json!({
            "access_token": access,
            "refresh_token": refresh,
            "expires_in": 3600
        });
        let b = parse_kimi_token(&v, None, "dev").unwrap();
        assert_eq!(b.kind, "kimi");
        assert_eq!(b.account_id.as_deref(), Some("u1"));
        assert_eq!(b.email.as_deref(), Some("a@b.com"), "email 小写化");
        assert!(b.expires_at > now_ms(), "扣除余量后仍在未来");
        assert_eq!(b.device_id.as_deref(), Some("dev"));

        // refresh 缺失时回退旧值；身份可从旧 refresh 找回
        let v2 = json!({ "access_token": "not-a-jwt", "expires_in": 60 });
        let b2 = parse_kimi_token(&v2, Some("old-refresh"), "dev").unwrap();
        assert_eq!(b2.refresh, "old-refresh");

        assert!(parse_kimi_token(&json!({ "access_token": "a" }), None, "d").is_err());
        assert!(
            parse_kimi_token(&json!({ "access_token": "a", "expires_in": -5, "refresh_token": "r" }), None, "d").is_err(),
            "负 expires_in 拒绝"
        );
    }

    #[test]
    fn command_code_auth_url_carries_callback_and_state() {
        let url = command_code_auth_url(5959, "st");
        assert!(url.starts_with("https://commandcode.ai/studio/auth/cli?"));
        assert!(url.contains("callback=http%3A%2F%2F127.0.0.1%3A5959%2Fcallback"), "{url}");
        assert!(url.contains("state=st"));
    }

    #[test]
    fn paste_parsing_variants() {
        // 回调 JSON
        let ok = parse_command_code_paste(
            r#"{"apiKey":"k1","state":"s1","userId":"u","userName":"n","keyName":"x"}"#,
            "s1",
        )
        .unwrap();
        assert!(matches!(ok, CommandCodePaste::Callback(ref cb) if cb.api_key == "k1"));
        // state 不匹配拒绝
        assert!(parse_command_code_paste(r#"{"apiKey":"k1","state":"old"}"#, "s1").is_err());
        // URL 形态（query 与 fragment 均可）
        let ok = parse_command_code_paste("https://commandcode.ai/done?apiKey=k2&state=s1", "s1").unwrap();
        assert!(matches!(ok, CommandCodePaste::Callback(ref cb) if cb.api_key == "k2"));
        let ok = parse_command_code_paste("https://commandcode.ai/done#state=s1&apiKey=k3", "s1").unwrap();
        assert!(matches!(ok, CommandCodePaste::Callback(ref cb) if cb.api_key == "k3"));
        assert!(parse_command_code_paste("https://commandcode.ai/done?apiKey=k2&state=bad", "s1").is_err());
        // 裸 key（豁免 state）与非法形态
        assert!(matches!(parse_command_code_paste("  sk-abc123 ", "s1").unwrap(), CommandCodePaste::RawKey(ref k) if k == "sk-abc123"));
        assert!(parse_command_code_paste("not a key", "s1").is_err());
        assert!(parse_command_code_paste("", "s1").is_err());
    }
}
