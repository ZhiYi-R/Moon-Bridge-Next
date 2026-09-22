//! 认证能力引擎：`CAP_AUTH` 插件的宿主侧驱动（平台无关，core 零平台代码）。
//!
//! 契约（与 `plugins/auth/*.lua` 及 LSP 桩 `plugins/moonbridge.lua` 一致）：
//! - 令牌包（bundle）是**插件自有 JSON**，core 只认两个约定字段：`access`
//!   （非空字符串，出站凭据）与 `expires_at`（unix 毫秒，缺省 = 永不过期）。
//!   平台私有字段（refresh、device_id 等）原样透传，core 不解释。
//! - 过期判定只在 core 做一次 skew（提前 5 分钟视为过期）；插件写入的
//!   `expires_at` 必须是服务端原始过期时刻，不得预先扣减。
//! - 令牌包持久化在 store 的 secrets 表（scope = `provider:{key}`，key = `oauth`），
//!   经 `EncKey` 加密；不进 `provider.extra`、不回传前端、不落日志。
//! - 刷新由 host 按 provider 键**单飞**（并发请求不双刷同一 refresh token）；
//!   刷新 HTTP 在 Lua 内经 `mb.http` 发出（30s 兜底超时），host 持锁期间不做
//!   任何无超时网络等待。刷新失败：旧令牌在有效期内回退使用并记 60s 退避；
//!   上游 401 触发一次强制刷新重发（见 dispatch）。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use moonbridge_plugin::{
    CallbackHandle, CallbackSpec, HostBridge, LuaRuntime, SandboxLimits, SessionStore,
};
use moonbridge_store::Database;
use serde_json::Value;
use tokio::sync::Notify;

use crate::state::AppState;

/// 过期安全余量：提前 5 分钟视为过期（只在此处扣一次，插件侧不得再扣）。
pub const EXPIRY_SKEW_MS: i64 = 5 * 60 * 1000;

/// 刷新失败后的重试退避（期间内沿用仍有效的旧令牌）。
const REFRESH_BACKOFF_MS: i64 = 60 * 1000;

/// secrets 表中令牌包的 key。
pub const BUNDLE_KEY: &str = "oauth";

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 令牌包在 secrets 表中的 scope。
pub fn provider_scope(provider_key: &str) -> String {
    format!("provider:{provider_key}")
}

// ───────────────────────── 令牌包契约 ─────────────────────────

/// 校验令牌包形态：对象、`access` 非空字符串、`expires_at`（若有）为正数。
pub fn validate_bundle(b: &Value) -> Result<(), String> {
    let obj = b.as_object().ok_or("令牌包必须是 JSON 对象")?;
    match obj.get("access").and_then(Value::as_str) {
        Some(a) if !a.is_empty() => {}
        _ => return Err("令牌包缺少有效 access 字段".to_string()),
    }
    if let Some(e) = obj.get("expires_at") {
        match e.as_i64() {
            Some(v) if v > 0 => {}
            _ => return Err("令牌包 expires_at 必须是正数（unix 毫秒）".to_string()),
        }
    }
    Ok(())
}

/// 是否临期（缺省 expires_at = 永不过期；skew 只算这一次）。
pub fn needs_refresh(b: &Value, now_ms: i64) -> bool {
    match b.get("expires_at").and_then(Value::as_i64) {
        Some(exp) => exp - EXPIRY_SKEW_MS <= now_ms,
        None => false,
    }
}

/// 是否仍在有效期内（不含 skew；用于刷新失败时的旧令牌回退判定）。
fn still_valid(b: &Value, now_ms: i64) -> bool {
    match b.get("expires_at").and_then(Value::as_i64) {
        Some(exp) => exp > now_ms,
        None => true,
    }
}

/// 从 secrets 表读令牌包（未登录/损坏均为明确错误）。
fn read_bundle(db: &Database, provider_key: &str) -> Result<Value, String> {
    let raw = db
        .secret_get(&provider_scope(provider_key), BUNDLE_KEY)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("上游 {provider_key} 未登录（无认证令牌），请先在 UI 完成登录"))?;
    let b: Value = serde_json::from_str(&raw).map_err(|e| format!("令牌包损坏: {e}"))?;
    validate_bundle(&b)?;
    Ok(b)
}

/// 把令牌包写回 secrets 表（调用方须先 `validate_bundle`）。
pub fn write_bundle(db: &Database, provider_key: &str, bundle: &Value) -> Result<(), String> {
    db.secret_set(
        &provider_scope(provider_key),
        BUNDLE_KEY,
        &bundle.to_string(),
    )
    .map_err(|e| e.to_string())
}

/// 插件 `auth_headers` 返回值的解析：`{{k,v},...}` 保序数组。
///
/// 逐对校验：键必须合法 header token、值不得含 CR/LF（防响应拆分注入）。
pub fn parse_auth_headers(v: &Value) -> Result<Vec<(String, String)>, String> {
    let arr = v
        .as_array()
        .ok_or("auth_headers 必须返回 {{k,v},...} 数组")?;
    let mut out = Vec::with_capacity(arr.len());
    for pair in arr {
        let p = pair.as_array().ok_or("auth_headers 元素必须是 {k,v} 对")?;
        if p.len() != 2 {
            return Err("auth_headers 元素必须恰为两个元素".to_string());
        }
        let k = p[0].as_str().ok_or("header 名必须是字符串")?;
        let v = p[1].as_str().ok_or("header 值必须是字符串")?;
        if k.is_empty()
            || !k
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "!#$%&'*+-.^_`|~".contains(c))
        {
            return Err(format!("header 名非法: {k}"));
        }
        if v.contains('\r') || v.contains('\n') {
            return Err(format!("header 值含换行（响应拆分风险）: {k}"));
        }
        out.push((k.to_string(), v.to_string()));
    }
    Ok(out)
}

/// 把认证头并入上游请求头（大小写不敏感替换，插件返回的头优先）。
pub fn merge_headers(headers: &mut Vec<(String, String)>, extra: &[(String, String)]) {
    for (k, v) in extra {
        if let Some(e) = headers
            .iter_mut()
            .find(|(ek, _)| ek.eq_ignore_ascii_case(k))
        {
            e.1 = v.clone();
        } else {
            headers.push((k.clone(), v.clone()));
        }
    }
}

/// 调插件函数的 ctx：provider 键 + 宿主平台信息（沙箱无 os 库，Kimi 的
/// X-Msh-Device-* 身份头需要主机名/OS/架构）。
pub fn auth_ctx(provider_key: &str) -> Value {
    auth_ctx_with(provider_key, None)
}

/// 带登录来源选择的 ctx（编排器把用户在 UI 选的来源透传给 auth_begin）。
pub fn auth_ctx_with(provider_key: &str, source: Option<&str>) -> Value {
    let hostname = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_default();
    let mut ctx = serde_json::json!({
        "provider": provider_key,
        "host": {
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "name": hostname,
        }
    });
    if let Some(s) = source {
        ctx["source"] = serde_json::json!(s);
    }
    ctx
}

// ───────────────────────── 出站集成 ─────────────────────────

/// 出站前取认证头：读令牌包 → 临期则单飞刷新 → 调 `MB.auth_headers`。
///
/// 任一环节失败都是**显式错误**（调用方按路由失败处理），不静默降级为无认证
/// 请求——上游会以更费解的 401 拒绝，且丢失「未登录/刷新失败」的真实原因。
pub async fn prepare_headers(
    state: &Arc<AppState>,
    rt: &Arc<LuaRuntime>,
    provider_key: &str,
) -> Result<Vec<(String, String)>, String> {
    let mut bundle = read_bundle(&state.db, provider_key)?;
    if needs_refresh(&bundle, now_ms()) {
        bundle = refresh_single_flight(state, rt, provider_key, bundle).await?;
    }
    let v = rt
        .call_mb("auth_headers", &[auth_ctx(provider_key), bundle])
        .await
        .map_err(|e| format!("auth_headers 调用失败: {e}"))?;
    parse_auth_headers(&v)
}

/// 401 后的强制刷新：无条件调 `MB.auth_refresh`（同样单飞），成功后返回新认证头。
/// 失败返回错误——调用方保留原 401 响应走常规错误路径。
pub async fn force_refresh_headers(
    state: &Arc<AppState>,
    rt: &Arc<LuaRuntime>,
    provider_key: &str,
) -> Result<Vec<(String, String)>, String> {
    let bundle = read_bundle(&state.db, provider_key)?;
    let bundle = do_refresh(state, rt, provider_key, bundle).await?;
    let v = rt
        .call_mb("auth_headers", &[auth_ctx(provider_key), bundle])
        .await
        .map_err(|e| format!("auth_headers 调用失败: {e}"))?;
    parse_auth_headers(&v)
}

/// 单飞刷新：按 provider 键串行；双检、退避、失败回退旧令牌。
async fn refresh_single_flight(
    state: &Arc<AppState>,
    rt: &Arc<LuaRuntime>,
    provider_key: &str,
    bundle: Value,
) -> Result<Value, String> {
    let lock = {
        let mut g = state.auth_locks.lock().unwrap();
        g.entry(provider_key.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    };
    let _guard = lock.lock().await;
    // 双检：等锁期间可能已被另一请求刷新
    let cur = read_bundle(&state.db, provider_key).unwrap_or(bundle);
    if !needs_refresh(&cur, now_ms()) {
        return Ok(cur);
    }
    // 退避：上次刷新刚失败过，旧令牌仍有效则沿用
    let now = now_ms();
    let backoff_until = state
        .auth_backoff
        .lock()
        .unwrap()
        .get(provider_key)
        .copied()
        .unwrap_or(0);
    if now < backoff_until && still_valid(&cur, now) {
        return Ok(cur);
    }
    do_refresh(state, rt, provider_key, cur).await
}

/// 实际刷新（调用方已持单飞锁或为 401 强制路径）。
async fn do_refresh(
    state: &Arc<AppState>,
    rt: &Arc<LuaRuntime>,
    provider_key: &str,
    cur: Value,
) -> Result<Value, String> {
    match rt
        .call_mb("auth_refresh", &[auth_ctx(provider_key), cur.clone()])
        .await
    {
        Ok(newb) => {
            validate_bundle(&newb)?;
            write_bundle(&state.db, provider_key, &newb)?;
            state.auth_backoff.lock().unwrap().remove(provider_key);
            tracing::info!(provider = %provider_key, plugin = %rt.name, "认证令牌已刷新");
            Ok(newb)
        }
        Err(e) => {
            let now = now_ms();
            // 失败回退：旧令牌在有效期内继续使用，记退避避免每请求都撞刷新
            if still_valid(&cur, now) {
                state
                    .auth_backoff
                    .lock()
                    .unwrap()
                    .insert(provider_key.to_string(), now + REFRESH_BACKOFF_MS);
                tracing::warn!(provider = %provider_key, error = %e, "令牌刷新失败，退避期内沿用旧令牌");
                Ok(cur)
            } else {
                Err(format!("认证令牌已过期且刷新失败（请重新登录）: {e}"))
            }
        }
    }
}

// ───────────────────────── 回环回调监听 ─────────────────────────

/// 单次回调的结果槽 + 到达通知。
struct CallbackEntry {
    result: Arc<tokio::sync::Mutex<Option<Value>>>,
    notify: Arc<Notify>,
    shutdown: tokio::sync::oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}

/// 活跃回调监听注册表（id → 监听项）。插件不能自己 bind 端口，回环监听一律
/// 由宿主托管：一次性送达、关闭/超时/注册表销毁即清理。
#[derive(Default)]
pub struct CallbackRegistry {
    inner: std::sync::Mutex<HashMap<String, CallbackEntry>>,
}

impl CallbackRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// 启动一次性监听：首选 `spec.preferred_port`，占用则随机端口。
    ///
    /// 仅接受 `POST {spec.path}`（JSON body 原样交出，state 等校验由插件在
    /// Lua 侧完成）；`spec.origins` 非空时 CORS 钉死这些源（浏览器页面内
    /// fetch 回调的预检需要）。
    pub async fn listen(self: &Arc<Self>, spec: CallbackSpec) -> Result<CallbackHandle, String> {
        use axum::{routing::post, Json, Router};
        use tower_http::cors::CorsLayer;

        let id = uuid::Uuid::new_v4().simple().to_string();
        let result = Arc::new(tokio::sync::Mutex::new(Option::<Value>::None));
        let notify = Arc::new(Notify::new());
        let (sd_tx, sd_rx) = tokio::sync::oneshot::channel::<()>();

        let shared = result.clone();
        let notify2 = notify.clone();
        let handler = move |Json(body): Json<Value>| {
            let shared = shared.clone();
            let notify = notify2.clone();
            async move {
                let mut slot = shared.lock().await;
                if slot.is_none() {
                    *slot = Some(body);
                    notify.notify_waiters();
                    Json(serde_json::json!({ "success": true }))
                } else {
                    Json(
                        serde_json::json!({ "success": false, "error": "callback already consumed" }),
                    )
                }
            }
        };
        let mut app = Router::new().route(&spec.path, post(handler));
        if !spec.origins.is_empty() {
            let origins: Vec<http::HeaderValue> = spec
                .origins
                .iter()
                .filter_map(|o| http::HeaderValue::from_str(o).ok())
                .collect();
            if origins.len() != spec.origins.len() {
                return Err("CORS 允许源含非法值".to_string());
            }
            app = app.layer(
                CorsLayer::new()
                    .allow_origin(tower_http::cors::AllowOrigin::list(origins))
                    .allow_methods([http::Method::POST, http::Method::OPTIONS])
                    .allow_headers([http::header::CONTENT_TYPE]),
            );
        }

        let bind = match spec.preferred_port {
            Some(p) => match tokio::net::TcpListener::bind(("127.0.0.1", p)).await {
                Ok(l) => l,
                Err(_) => tokio::net::TcpListener::bind(("127.0.0.1", 0))
                    .await
                    .map_err(|e| format!("回调端口绑定失败: {e}"))?,
            },
            None => tokio::net::TcpListener::bind(("127.0.0.1", 0))
                .await
                .map_err(|e| format!("回调端口绑定失败: {e}"))?,
        };
        let port = bind
            .local_addr()
            .map_err(|e| format!("读取回调端口失败: {e}"))?
            .port();
        let task = tokio::spawn(async move {
            let _ = axum::serve(bind, app)
                .with_graceful_shutdown(async move {
                    let _ = sd_rx.await;
                })
                .await;
        });
        self.inner.lock().unwrap().insert(
            id.clone(),
            CallbackEntry {
                result,
                notify,
                shutdown: sd_tx,
                task,
            },
        );
        Ok(CallbackHandle {
            id,
            port,
            url: format!("http://127.0.0.1:{port}{}", spec.path),
        })
    }

    /// 等待回调参数；超时返回 `Ok(None)`（监听保持存活，可再次等待）。
    pub async fn await_callback(
        &self,
        id: &str,
        timeout: Duration,
    ) -> Result<Option<Value>, String> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let (result, notify) = {
                let g = self.inner.lock().unwrap();
                let Some(e) = g.get(id) else {
                    return Err(format!("回调监听不存在或已关闭: {id}"));
                };
                (e.result.clone(), e.notify.clone())
            };
            if let Some(v) = result.lock().await.clone() {
                return Ok(Some(v));
            }
            let remain = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remain.is_zero() {
                return Ok(None);
            }
            let _ = tokio::time::timeout(remain, notify.notified()).await;
        }
    }

    /// 关闭监听（幂等）：停服务器、移除表项。
    pub fn close(&self, id: &str) {
        if let Some(e) = self.inner.lock().unwrap().remove(id) {
            let _ = e.shutdown.send(());
            e.task.abort();
        }
    }

    /// 关闭全部监听（注册表销毁时兜底）。
    pub fn close_all(&self) {
        let entries: Vec<CallbackEntry> = {
            let mut g = self.inner.lock().unwrap();
            g.drain().map(|(_, e)| e).collect()
        };
        for e in entries {
            let _ = e.shutdown.send(());
            e.task.abort();
        }
    }
}

impl Drop for CallbackRegistry {
    fn drop(&mut self) {
        self.close_all();
    }
}

// ───────────────────────── 一次性运行时（登录编排用） ─────────────────────────

/// 从 store 加载单个插件为一次性运行时（登录编排等旁路场景；不走网关注册表）。
///
/// 脚本解析与网关加载同口径（`parse_script_ref` 的 plugins_dir 包含性校验）。
pub fn load_plugin_runtime(
    db: &Database,
    name: &str,
    host: Arc<dyn HostBridge>,
    plugins_dir: Option<&std::path::Path>,
    limits: SandboxLimits,
) -> Result<Arc<LuaRuntime>, String> {
    let rec = db
        .get_plugin(name)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("插件不存在: {name}"))?;
    let script = crate::read_script(&rec.script_ref, plugins_dir)
        .ok_or_else(|| format!("插件脚本不可读: {name}"))?;
    let rt = LuaRuntime::new_with_limits(
        name,
        &script,
        &rec.config,
        host,
        SessionStore::new(),
        limits,
    )
    .map_err(|e| e.to_string())?;
    Ok(Arc::new(rt))
}

#[cfg(test)]
mod tests {
    use super::*;
    use moonbridge_plugin::LuaRuntime;
    use moonbridge_protocol::{builtin_registry, NoopHooks};
    use moonbridge_store::Database;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::bridge::GatewayBridge;
    use crate::config::GatewayConfig;

    // ── 测试替身：按脚本应答的上游 mock（axum，与 e2e 同风格）──

    /// 顺序应答 mock：每次命中弹出下一个响应（末个之后重复末个），并计数。
    async fn spawn_scripted(steps: Vec<(u16, Value)>) -> (String, Arc<AtomicUsize>) {
        use axum::{routing::post, Json, Router};
        let hits = Arc::new(AtomicUsize::new(0));
        let steps = Arc::new(tokio::sync::Mutex::new(steps));
        let hits2 = hits.clone();
        let app = Router::new().fallback(post(move || {
            let steps = steps.clone();
            let hits = hits2.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                let mut g = steps.lock().await;
                let (status, body) = if g.len() > 1 {
                    g.remove(0)
                } else {
                    g[0].clone()
                };
                (
                    axum::http::StatusCode::from_u16(status).unwrap(),
                    Json(body),
                )
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}"), hits)
    }

    fn test_state(db: Arc<Database>) -> Arc<AppState> {
        AppState::new(
            GatewayConfig::default(),
            db,
            Arc::new(builtin_registry()),
            Arc::new(NoopHooks),
            None,
            reqwest::Client::new(),
            CallbackRegistry::new(),
        )
    }

    fn test_bridge(db: Arc<Database>) -> Arc<dyn HostBridge> {
        Arc::new(GatewayBridge::new(
            reqwest::Client::new(),
            reqwest::Client::new(),
            db.clone(),
            Arc::new(builtin_registry()),
            CallbackRegistry::new(),
            Duration::from_secs(10),
        ))
    }

    /// 通用假平台插件：device_code 流 + 刷新 + 头产出（全部走 mb.http）。
    fn fake_platform_rt(db: Arc<Database>, base: &str) -> Arc<LuaRuntime> {
        let script = r##"
MB = { name = "auth-fake", capabilities = { "auth" } }

function MB.auth_begin(ctx)
  local r = mb.http.request({ method = "POST", url = mb.config.base .. "/device", body = {} })
  if r.status ~= 200 then error("device authorize failed: " .. r.status) end
  return {
    kind = "device_code",
    verification_url = r.body.verification_uri,
    user_code = r.body.user_code,
    interval_secs = 1,
    handle = { device_code = r.body.device_code },
  }
end

function MB.auth_poll(ctx, handle)
  local r = mb.http.request({ method = "POST", url = mb.config.base .. "/token", body = { device_code = handle.device_code } })
  if r.status == 200 then
    return { status = "done", bundle = {
      access = r.body.access_token,
      refresh = r.body.refresh_token,
      expires_at = r.body.expires_at,
    } }
  end
  local err = r.body and r.body.error
  if err == "authorization_pending" then return { status = "pending" } end
  if err == "slow_down" then return { status = "slow_down" } end
  return { status = "error", message = "token poll failed: " .. r.status }
end

function MB.auth_refresh(ctx, bundle)
  local r = mb.http.request({ method = "POST", url = mb.config.base .. "/refresh", body = { refresh = bundle.refresh } })
  if r.status ~= 200 then error("refresh rejected: " .. r.status) end
  return { access = r.body.access_token, refresh = bundle.refresh, expires_at = r.body.expires_at }
end

function MB.auth_headers(ctx, bundle)
  return { { "authorization", "Bearer " .. bundle.access } }
end
"##;
        Arc::new(
            LuaRuntime::new(
                "auth-fake",
                script,
                &json!({ "base": base }),
                test_bridge(db),
                SessionStore::new(),
            )
            .unwrap(),
        )
    }

    fn fresh_bundle(access: &str, expires_in_ms: i64) -> Value {
        json!({
            "access": access,
            "refresh": "r1",
            "expires_at": now_ms() + expires_in_ms,
        })
    }

    // ── 契约纯函数 ──

    #[test]
    fn bundle_contract_single_skew() {
        validate_bundle(&json!({"access": "a"})).unwrap();
        validate_bundle(&json!({"access": "a", "expires_at": 123})).unwrap();
        assert!(validate_bundle(&json!({"access": ""})).is_err());
        assert!(validate_bundle(&json!({"refresh": "r"})).is_err());
        assert!(validate_bundle(&json!({"access": "a", "expires_at": -1})).is_err());
        assert!(validate_bundle(&json!("not-object")).is_err());
        let now = now_ms();
        // 缺省 expires_at = 永不过期
        assert!(!needs_refresh(&json!({"access": "a"}), now));
        // skew 边界（只算一次：exp - SKEW <= now 即临期）
        assert!(needs_refresh(
            &json!({"access": "a", "expires_at": now + EXPIRY_SKEW_MS}),
            now
        ));
        assert!(!needs_refresh(
            &json!({"access": "a", "expires_at": now + EXPIRY_SKEW_MS + 1000}),
            now
        ));
        assert!(needs_refresh(
            &json!({"access": "a", "expires_at": now - 1}),
            now
        ));
    }

    #[test]
    fn parse_auth_headers_validation() {
        let ok = parse_auth_headers(&json!([
            ["authorization", "Bearer x"],
            ["x-msh-device-id", "dev-1"]
        ]))
        .unwrap();
        assert_eq!(ok.len(), 2);
        assert_eq!(ok[0], ("authorization".to_string(), "Bearer x".to_string()));
        assert!(parse_auth_headers(&json!({"a": "b"})).is_err());
        assert!(parse_auth_headers(&json!(["a", "b"])).is_err());
        assert!(parse_auth_headers(&json!([["a"]])).is_err());
        assert!(parse_auth_headers(&json!([["a", "b", "c"]])).is_err());
        // header 名/值注入
        assert!(parse_auth_headers(&json!([["bad name", "v"]])).is_err());
        assert!(parse_auth_headers(&json!([["x-ok", "a\r\nHost: evil"]])).is_err());
    }

    #[test]
    fn merge_headers_replaces_case_insensitively() {
        let mut h = vec![
            ("Authorization".to_string(), "Bearer old".to_string()),
            ("content-type".to_string(), "application/json".to_string()),
        ];
        merge_headers(
            &mut h,
            &[("authorization".to_string(), "Bearer new".to_string())],
        );
        assert_eq!(h.len(), 2, "替换不产生重复头");
        assert_eq!(h[0].1, "Bearer new");
        merge_headers(&mut h, &[("x-msh-device-id".to_string(), "d1".to_string())]);
        assert_eq!(h.len(), 3);
    }

    // ── 回调监听 ──

    #[tokio::test]
    async fn callback_listener_once_await_close() {
        let reg = CallbackRegistry::new();
        let h = reg
            .listen(CallbackSpec {
                path: "/callback".to_string(),
                origins: vec!["https://studio.example.com".to_string()],
                preferred_port: None,
            })
            .await
            .unwrap();
        assert!(h
            .url
            .starts_with(&format!("http://127.0.0.1:{}/callback", h.port)));
        let client = reqwest::Client::new();
        let resp = client
            .post(&h.url)
            .header("origin", "https://studio.example.com")
            .json(&json!({"apiKey": "k1", "state": "s1"}))
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_success());
        let got = reg
            .await_callback(&h.id, Duration::from_millis(500))
            .await
            .unwrap()
            .expect("回调应送达");
        assert_eq!(got["apiKey"], "k1");
        // 第二次 POST：已消费
        let resp2 = client
            .post(&h.url)
            .json(&json!({"apiKey": "k2"}))
            .send()
            .await
            .unwrap();
        let v2: Value = resp2.json().await.unwrap();
        assert_eq!(v2["success"], false);
        // 关闭后再等待：明确错误而不是悬挂
        reg.close(&h.id);
        assert!(reg
            .await_callback(&h.id, Duration::from_millis(50))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn callback_listener_timeout_then_still_alive() {
        let reg = CallbackRegistry::new();
        let h = reg
            .listen(CallbackSpec {
                path: "/cb".to_string(),
                origins: vec![],
                preferred_port: None,
            })
            .await
            .unwrap();
        // 短超时未到达 → None，监听仍存活可再等待
        assert!(reg
            .await_callback(&h.id, Duration::from_millis(50))
            .await
            .unwrap()
            .is_none());
        let reg2 = reg.clone();
        let id = h.id.clone();
        let waiter =
            tokio::spawn(async move { reg2.await_callback(&id, Duration::from_secs(2)).await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        reqwest::Client::new()
            .post(&h.url)
            .json(&json!({"ok": true}))
            .send()
            .await
            .unwrap();
        let got = waiter.await.unwrap().unwrap();
        assert_eq!(got.unwrap()["ok"], true);
        reg.close(&h.id);
    }

    // ── device_code 状态机（mock HTTP 端到端）──

    #[tokio::test]
    async fn device_code_flow_pending_then_done() {
        let (base, _hits) = spawn_scripted(vec![
            (200, json!({"verification_uri": "https://x.example/verify", "user_code": "ABCD", "device_code": "dc-1"})),
            (400, json!({"error": "authorization_pending"})),
            (200, json!({"access_token": "tok-1", "refresh_token": "ref-1", "expires_at": now_ms() + 3_600_000})),
        ]).await;
        let db = Arc::new(Database::open_in_memory().unwrap());
        let rt = fake_platform_rt(db.clone(), &base);

        let begin = rt.call_mb_once("auth_begin", &json!({})).await.unwrap();
        assert_eq!(begin["user_code"], "ABCD");
        assert_eq!(begin["handle"]["device_code"], "dc-1");
        let handle = begin["handle"].clone();

        let p1 = rt
            .call_mb("auth_poll", &[json!({}), handle.clone()])
            .await
            .unwrap();
        assert_eq!(p1["status"], "pending");
        let p2 = rt.call_mb("auth_poll", &[json!({}), handle]).await.unwrap();
        assert_eq!(p2["status"], "done");
        let bundle = &p2["bundle"];
        validate_bundle(bundle).unwrap();
        assert_eq!(bundle["access"], "tok-1");

        let h = rt
            .call_mb("auth_headers", &[json!({}), bundle.clone()])
            .await
            .unwrap();
        let headers = parse_auth_headers(&h).unwrap();
        assert_eq!(
            headers,
            vec![("authorization".to_string(), "Bearer tok-1".to_string())]
        );
    }

    // ── 单飞刷新：N 并发只刷一次 ──

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn single_flight_refresh_under_concurrency() {
        let (base, hits) = spawn_scripted(vec![(
            200,
            json!({"access_token": "fresh-1", "expires_at": now_ms() + 3_600_000}),
        )])
        .await;
        let db = Arc::new(Database::open_in_memory().unwrap());
        let state = test_state(db.clone());
        let rt = fake_platform_rt(db.clone(), &base);
        // 已过期令牌
        write_bundle(&db, "k", &fresh_bundle("stale", -1000)).unwrap();

        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..8 {
            let state = state.clone();
            let rt = rt.clone();
            tasks.spawn(async move { prepare_headers(&state, &rt, "k").await });
        }
        let mut results = Vec::new();
        while let Some(r) = tasks.join_next().await {
            results.push(r.unwrap());
        }
        for r in &results {
            let headers = r.as_ref().expect("全部并发调用应成功");
            assert_eq!(
                headers,
                &vec![("authorization".to_string(), "Bearer fresh-1".to_string())]
            );
        }
        assert_eq!(hits.load(Ordering::SeqCst), 1, "单飞：8 并发只刷一次");
        let b = read_bundle(&db, "k").unwrap();
        assert_eq!(b["access"], "fresh-1");
    }

    // ── 刷新失败：回退旧令牌 + 退避 ──

    #[tokio::test]
    async fn refresh_failure_falls_back_and_backs_off() {
        let (base, hits) = spawn_scripted(vec![(500, json!({"error": "server down"}))]).await;
        let db = Arc::new(Database::open_in_memory().unwrap());
        let state = test_state(db.clone());
        let rt = fake_platform_rt(db.clone(), &base);
        // 临期但仍有效（60s 后到期，skew 5min → 需要刷新）
        write_bundle(&db, "k", &fresh_bundle("old-but-valid", 60_000)).unwrap();

        let headers = prepare_headers(&state, &rt, "k").await.unwrap();
        assert_eq!(headers[0].1, "Bearer old-but-valid", "刷新失败回退旧令牌");
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        // 退避期内再次调用：不再撞刷新端点
        let headers2 = prepare_headers(&state, &rt, "k").await.unwrap();
        assert_eq!(headers2[0].1, "Bearer old-but-valid");
        assert_eq!(hits.load(Ordering::SeqCst), 1, "退避期内不得重复刷新");
    }

    #[tokio::test]
    async fn refresh_failure_with_expired_bundle_is_explicit_error() {
        let (base, _hits) = spawn_scripted(vec![(400, json!({"error": "invalid_grant"}))]).await;
        let db = Arc::new(Database::open_in_memory().unwrap());
        let state = test_state(db.clone());
        let rt = fake_platform_rt(db.clone(), &base);
        // 已彻底过期
        write_bundle(&db, "k", &fresh_bundle("dead", -3_600_000)).unwrap();
        let err = prepare_headers(&state, &rt, "k").await.unwrap_err();
        assert!(err.contains("刷新失败"), "{err}");
    }

    // ── 401 强制刷新：未过期也刷 ──

    #[tokio::test]
    async fn force_refresh_ignores_expiry() {
        let (base, hits) = spawn_scripted(vec![(
            200,
            json!({"access_token": "forced", "expires_at": now_ms() + 3_600_000}),
        )])
        .await;
        let db = Arc::new(Database::open_in_memory().unwrap());
        let state = test_state(db.clone());
        let rt = fake_platform_rt(db.clone(), &base);
        // 远未过期（prepare_headers 不会刷）
        write_bundle(&db, "k", &fresh_bundle("still-good", 86_400_000)).unwrap();
        let headers = force_refresh_headers(&state, &rt, "k").await.unwrap();
        assert_eq!(headers[0].1, "Bearer forced");
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert_eq!(read_bundle(&db, "k").unwrap()["access"], "forced");
    }

    // ── 未登录：明确错误 ──

    #[tokio::test]
    async fn missing_bundle_is_explicit_not_silent() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let state = test_state(db.clone());
        let rt = fake_platform_rt(db.clone(), "http://127.0.0.1:1");
        let err = prepare_headers(&state, &rt, "ghost").await.unwrap_err();
        assert!(err.contains("未登录"), "{err}");
    }

    // ── fs_read 的 home 包含性复查（纵深防御）──

    #[tokio::test]
    async fn fs_read_rejects_outside_home() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let bridge = test_bridge(db);
        #[cfg(windows)]
        let outside = "C:\\Windows\\notepad.exe";
        #[cfg(not(windows))]
        let outside = "/etc/hostname";
        let err = bridge.fs_read(outside, 1024).await.unwrap_err();
        assert!(err.contains("home"), "{err}");
    }
}
