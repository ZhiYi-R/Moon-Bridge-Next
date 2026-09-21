//! 真实插件文件（`plugins/auth/*.lua`）的端到端测试：device_code / callback /
//! paste 三流各配 axum mock（顺序应答 + 命中计数，与 e2e.rs 同风格）。
//!
//! 插件的端点注入走 `mb.config.*_override`——测试用 mock 主机替换真实
//! auth.kimi.com / api.commandcode.ai，生产种子配置为空（真实端点）。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::{Json, Router};
use moonbridge_gateway::bridge::GatewayBridge;
use moonbridge_gateway::oauth::CallbackRegistry;
use moonbridge_plugin::{HostBridge, LuaRuntime, SessionStore};
use moonbridge_protocol::builtin_registry;
use moonbridge_store::Database;
use serde_json::{json, Value};

const KIMI: &str = include_str!("../../../plugins/auth/kimi.lua");
const COMMANDCODE: &str = include_str!("../../../plugins/auth/commandcode.lua");

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn test_db() -> Arc<Database> {
    Arc::new(Database::open_in_memory().unwrap())
}

fn test_bridge(db: Arc<Database>) -> Arc<dyn HostBridge> {
    Arc::new(GatewayBridge::new(
        reqwest::Client::new(),
        reqwest::Client::new(),
        db,
        Arc::new(builtin_registry()),
        CallbackRegistry::new(),
        Duration::from_secs(10),
    ))
}

fn load(name: &str, script: &str, config: Value, db: Arc<Database>) -> Arc<LuaRuntime> {
    Arc::new(LuaRuntime::new(name, script, &config, test_bridge(db), SessionStore::new()).unwrap())
}

/// 顺序应答 mock（fallback，任意方法/路径）：弹出下一个响应（末个之后重复末个）。
async fn spawn_queue(steps: Vec<(u16, Value)>) -> (String, Arc<AtomicUsize>) {
    let hits = Arc::new(AtomicUsize::new(0));
    let steps = Arc::new(tokio::sync::Mutex::new(steps));
    let hits2 = hits.clone();
    let app = Router::new().fallback(move || {
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
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), hits)
}

// ───────────────────────── Kimi：device_code 流 ─────────────────────────

#[tokio::test]
async fn kimi_device_code_full_flow() {
    let (base, _hits) = spawn_queue(vec![
        (
            200,
            json!({
                "verification_uri_complete": "https://auth.example/verify?code=ABCD",
                "user_code": "ABCD", "device_code": "dc-1",
                "expires_in": 900, "interval": 1,
            }),
        ),
        (400, json!({"error": "authorization_pending"})),
        (400, json!({"error": "slow_down", "interval": 7})),
        (
            200,
            json!({"access_token": "tok-1", "refresh_token": "ref-1", "expires_in": 3600}),
        ),
        (200, json!({"access_token": "tok-2", "expires_in": 3600})),
    ])
    .await;
    let db = test_db();
    let rt = load("auth-kimi", KIMI, json!({"host_override": base}), db);

    let d = rt.call_mb_once("auth_describe", &json!({})).await.unwrap();
    assert_eq!(d["kind"], "device_code");

    let begin = rt.call_mb_once("auth_begin", &json!({})).await.unwrap();
    assert_eq!(begin["user_code"], "ABCD");
    assert_eq!(begin["handle"]["device_code"], "dc-1");
    let handle = begin["handle"].clone();

    let p1 = rt
        .call_mb("auth_poll", &[json!({}), handle.clone()])
        .await
        .unwrap();
    assert_eq!(p1["status"], "pending");
    let p2 = rt
        .call_mb("auth_poll", &[json!({}), handle.clone()])
        .await
        .unwrap();
    assert_eq!(p2["status"], "slow_down");
    assert_eq!(p2["interval_secs"], 7);
    let p3 = rt.call_mb("auth_poll", &[json!({}), handle]).await.unwrap();
    assert_eq!(p3["status"], "done");
    let bundle = &p3["bundle"];
    assert_eq!(bundle["access"], "tok-1");
    assert_eq!(bundle["refresh"], "ref-1");
    // expires_at 是服务端原始过期时刻（允许本机时钟微差）
    let exp = bundle["expires_at"].as_i64().unwrap();
    assert!((exp - now_ms() - 3_600_000).abs() < 5_000, "{exp}");
    // device_id 持久化：32 位 hex
    assert_eq!(bundle["device_id"].as_str().unwrap().len(), 32);

    let h = rt
        .call_mb("auth_headers", &[json!({}), bundle.clone()])
        .await
        .unwrap();
    let headers = moonbridge_gateway::oauth::parse_auth_headers(&h).unwrap();
    assert_eq!(
        headers,
        vec![("authorization".to_string(), "Bearer tok-1".to_string())]
    );

    // refresh：响应缺 refresh_token 时沿用旧 refresh，access 更新
    let b2 = rt
        .call_mb("auth_refresh", &[json!({}), bundle.clone()])
        .await
        .unwrap();
    assert_eq!(b2["access"], "tok-2");
    assert_eq!(b2["refresh"], "ref-1", "refresh 缺失时沿用旧值");
    assert_eq!(b2["device_id"], bundle["device_id"]);
}

#[tokio::test]
async fn kimi_poll_error_and_expired_states() {
    let (base, _hits) = spawn_queue(vec![
        (
            200,
            json!({"verification_uri": "https://x", "user_code": "C", "device_code": "dc-9"}),
        ),
        (400, json!({"error": "expired_token"})),
    ])
    .await;
    let db = test_db();
    let rt = load("auth-kimi", KIMI, json!({"host_override": base}), db);
    let begin = rt.call_mb_once("auth_begin", &json!({})).await.unwrap();
    let p = rt
        .call_mb("auth_poll", &[json!({}), begin["handle"].clone()])
        .await
        .unwrap();
    assert_eq!(p["status"], "expired");
}

// ───────────────────────── Command Code：callback / paste / 来源选择 ─────────────────────────

/// 起 commandcode 插件并显式走浏览器来源（跳过本地导入，保证测试确定性）。
async fn cc_begin_browser(db: Arc<Database>, whoami: &str) -> (Arc<LuaRuntime>, Value) {
    let rt = load(
        "auth-commandcode",
        COMMANDCODE,
        json!({"whoami_override": whoami, "studio_override": "https://studio.invalid"}),
        db,
    );
    let begin = rt
        .call_mb_once("auth_begin", &json!({"source": "browser"}))
        .await
        .unwrap();
    assert!(
        begin["verification_url"]
            .as_str()
            .unwrap()
            .starts_with("https://studio.invalid/studio/auth/cli?"),
        "{}",
        begin["verification_url"]
    );
    (rt, begin)
}

/// 从 begin.verification_url 还原回调地址（query 里 urlencode 过的 callback 参数）。
fn callback_url(begin: &Value) -> String {
    let url = begin["verification_url"].as_str().unwrap();
    let enc = url
        .split("callback=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap();
    enc.replace("%3A", ":").replace("%2F", "/")
}

#[tokio::test]
async fn commandcode_callback_flow() {
    // 本机若存在真实 CLI 凭据，whoami 探测会给 401 → 导入失败回落，不干扰断言
    let (whoami, _wh) = spawn_queue(vec![(401, json!({"error": "nope"}))]).await;
    let db = test_db();
    let (rt, begin) = cc_begin_browser(db, &whoami).await;
    let state = begin["handle"]["state"].as_str().unwrap().to_string();
    let cb_url = callback_url(&begin);

    let p = rt
        .call_mb(
            "auth_poll",
            &[json!({}), begin["handle"].clone(), json!(null)],
        )
        .await
        .unwrap();
    assert_eq!(p["status"], "pending");

    let resp = reqwest::Client::new()
        .post(&cb_url)
        .json(&json!({"apiKey": "cc-key-1", "state": state, "userId": "u1", "userName": "n1"}))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());

    let p2 = rt
        .call_mb(
            "auth_poll",
            &[json!({}), begin["handle"].clone(), json!(null)],
        )
        .await
        .unwrap();
    assert_eq!(p2["status"], "done");
    let bundle = &p2["bundle"];
    assert_eq!(bundle["access"], "cc-key-1");
    assert_eq!(bundle["account"], "u1");
    // 长期 key：无 expires_at（core 永不触发刷新）
    assert!(bundle.get("expires_at").is_none());

    let h = rt
        .call_mb("auth_headers", &[json!({}), bundle.clone()])
        .await
        .unwrap();
    let headers = moonbridge_gateway::oauth::parse_auth_headers(&h).unwrap();
    assert_eq!(
        headers,
        vec![("authorization".to_string(), "Bearer cc-key-1".to_string())]
    );

    // auth_refresh：显式报错（长期 key 无刷新端点）
    assert!(rt
        .call_mb("auth_refresh", &[json!({}), bundle.clone()])
        .await
        .is_err());
}

#[tokio::test]
async fn commandcode_paste_raw_key_verified_then_done() {
    // whoami：先 401（裸 key 验证失败），后 200（重试成功）
    let (whoami, _wh) = spawn_queue(vec![
        (401, json!({"error": "bad key"})),
        (200, json!({"user": {"id": "u9", "userName": "nine"}})),
    ])
    .await;
    let db = test_db();
    let (rt, begin) = cc_begin_browser(db, &whoami).await;

    let p1 = rt
        .call_mb(
            "auth_poll",
            &[json!({}), begin["handle"].clone(), json!("sk-bad")],
        )
        .await
        .unwrap();
    assert_eq!(p1["status"], "pending");
    assert!(p1["message"].as_str().unwrap().contains("验证失败"), "{p1}");

    let p2 = rt
        .call_mb(
            "auth_poll",
            &[json!({}), begin["handle"].clone(), json!("sk-good")],
        )
        .await
        .unwrap();
    assert_eq!(p2["status"], "done");
    assert_eq!(p2["bundle"]["access"], "sk-good");
    assert_eq!(p2["bundle"]["account"], "u9");
}

#[tokio::test]
async fn commandcode_paste_callback_json_state_guard() {
    let (whoami, _wh) = spawn_queue(vec![(401, json!({"error": "nope"}))]).await;
    let db = test_db();
    let (rt, begin) = cc_begin_browser(db, &whoami).await;
    let state = begin["handle"]["state"].as_str().unwrap().to_string();

    let bad = rt
        .call_mb(
            "auth_poll",
            &[
                json!({}),
                begin["handle"].clone(),
                json!(r#"{"apiKey":"k1","state":"old"}"#),
            ],
        )
        .await
        .unwrap();
    assert_eq!(bad["status"], "pending");
    assert!(bad["message"].as_str().unwrap().contains("state"), "{bad}");

    let ok = rt
        .call_mb(
            "auth_poll",
            &[
                json!({}),
                begin["handle"].clone(),
                json!(format!(
                    r#"{{"apiKey":"k2","state":"{state}","userId":"u2"}}"#
                )),
            ],
        )
        .await
        .unwrap();
    assert_eq!(ok["status"], "done");
    assert_eq!(ok["bundle"]["access"], "k2");
    assert_eq!(ok["bundle"]["account"], "u2");
}

#[tokio::test]
async fn commandcode_local_cli_source_explicit_failure() {
    // 显式选择 local-cli 但导入失败（文件不存在或 whoami 401）→ 显式错误，不装成功
    let (whoami, _wh) = spawn_queue(vec![(401, json!({"error": "nope"}))]).await;
    let db = test_db();
    let rt = load(
        "auth-commandcode",
        COMMANDCODE,
        json!({"whoami_override": whoami}),
        db,
    );
    let begin = rt
        .call_mb_once("auth_begin", &json!({"source": "local-cli"}))
        .await
        .unwrap();
    assert_eq!(begin["status"], "error");
    let msg = begin["message"].as_str().unwrap();
    assert!(msg.contains("凭据") || msg.contains("whoami"), "{msg}");
}

#[tokio::test]
async fn commandcode_describe_advertises_sources() {
    let db = test_db();
    let rt = load("auth-commandcode", COMMANDCODE, json!({}), db);
    let d = rt.call_mb_once("auth_describe", &json!({})).await.unwrap();
    assert_eq!(d["kind"], "callback");
    assert_eq!(d["supports_paste"], true);
    let sources = d["sources"].as_array().unwrap();
    assert_eq!(sources.len(), 2);
    assert_eq!(sources[0]["id"], "local-cli");
    assert_eq!(sources[1]["id"], "browser");
}
