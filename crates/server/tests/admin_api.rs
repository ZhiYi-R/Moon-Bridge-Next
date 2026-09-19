//! `/api/*` 管理 REST API 的 HTTP 级集成测试。
//!
//! 全部走内存 Router + `tower::ServiceExt::oneshot`，由 fixture 显式标记监听状态。
//! 不绑定真实端口；重启请求只验证生命周期状态，不验证实际监听器重建。
//!
//! 每个测试用「进程 pid + tag」隔离的临时目录（不引入 tempfile 依赖，与
//! `src-tauri/src/state.rs` 的测试模式一致），`Harness` 析构时清理。

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use axum::Router;
use moonbridge_server::admin::{self, AdminState};
use moonbridge_server::config::{AppConfig, AppPaths};
use moonbridge_store::{
    Database, ModelDef, Offer, PluginRecord, Provider, Route, Setting, UsageRecord,
};
use serde_json::{json, Value};
use tower::ServiceExt;

/// 与 `Harness` 中注入的 `AdminState::admin_token` 一致。
const TOKEN: &str = "test-admin-token";

// ───────────────────────── 测试脚手架 ─────────────────────────

struct Harness {
    router: Router,
    db: Arc<Database>,
    paths: AppPaths,
    config: Arc<RwLock<AppConfig>>,
    persisted_config: Arc<RwLock<AppConfig>>,
    lifecycle: Arc<moonbridge_server::lifecycle::Lifecycle>,
    dir: PathBuf,
}

impl Harness {
    fn new(tag: &str) -> Self {
        Self::with_config(tag, AppConfig::default())
    }

    fn with_config(tag: &str, mut config: AppConfig) -> Self {
        config
            .gateway
            .auth_token
            .get_or_insert_with(|| "fixture-gateway-token".into());
        Self::with_configs(tag, config.clone(), config)
    }

    fn with_configs(tag: &str, config: AppConfig, persisted: AppConfig) -> Self {
        let gateway_token = config.gateway.auth_token.as_deref().unwrap();
        assert!(!gateway_token.trim().is_empty());
        assert_ne!(gateway_token.trim(), TOKEN);
        let dir = std::env::temp_dir().join(format!(
            "moonbridge-server-http-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let paths = AppPaths::resolve(dir.join("config"), dir.join("data"));
        paths.ensure_dirs().unwrap();
        persisted.save(&paths.config_file).unwrap();
        let config = Arc::new(RwLock::new(config));
        let persisted_config = Arc::new(RwLock::new(persisted));
        let lifecycle = Arc::new(moonbridge_server::lifecycle::Lifecycle::default());
        assert!(lifecycle.begin_start());
        lifecycle.listening("127.0.0.1:43210".parse().unwrap());
        let db = Arc::new(Database::open_in_memory().unwrap());
        let state = AdminState {
            db: db.clone(),
            paths: paths.clone(),
            config: config.clone(),
            persisted_config: persisted_config.clone(),
            lifecycle: lifecycle.clone(),
            admin_token: Arc::new(TOKEN.to_string()),
            catalog: reqwest::Client::new(),
        };
        let router = admin::router(state);
        Self {
            router,
            db,
            paths,
            config,
            persisted_config,
            lifecycle,
            dir,
        }
    }

    /// 带正确 Bearer token 的 JSON 调用。
    async fn json(&self, method: Method, uri: &str, body: Value) -> (StatusCode, Value) {
        let (status, raw) = self
            .raw(
                method,
                uri,
                Some(TOKEN),
                Some("application/json"),
                body.to_string().into_bytes(),
            )
            .await;
        (status, parse_json(&raw))
    }

    /// 带正确 Bearer token 的无体调用。
    async fn get(&self, uri: &str) -> (StatusCode, Value) {
        let (status, raw) = self
            .raw(Method::GET, uri, Some(TOKEN), None, Vec::new())
            .await;
        (status, parse_json(&raw))
    }

    /// 带正确 Bearer token 的无体调用，响应体按文本返回（用于 `text/plain` 端点）。
    async fn get_text(&self, uri: &str) -> (StatusCode, String) {
        self.raw(Method::GET, uri, Some(TOKEN), None, Vec::new())
            .await
    }

    async fn delete(&self, uri: &str) -> (StatusCode, Value) {
        let (status, raw) = self
            .raw(Method::DELETE, uri, Some(TOKEN), None, Vec::new())
            .await;
        (status, parse_json(&raw))
    }

    async fn raw(
        &self,
        method: Method,
        uri: &str,
        token: Option<&str>,
        content_type: Option<&str>,
        body: Vec<u8>,
    ) -> (StatusCode, String) {
        let mut builder = Request::builder().method(method).uri(uri);
        if let Some(token) = token {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        if let Some(ct) = content_type {
            builder = builder.header(header::CONTENT_TYPE, ct);
        }
        let request = builder.body(Body::from(body)).unwrap();
        let response = self
            .router
            .clone()
            .oneshot(request)
            .await
            .expect("Router 应能应答请求");
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("读取响应体失败");
        (status, String::from_utf8_lossy(&bytes).to_string())
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn parse_json(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap_or_else(|e| panic!("响应体不是合法 JSON ({e}): {raw}"))
}

/// 断言错误响应：状态码匹配且体为 `{"message": ...}`。
fn assert_error(status: StatusCode, body: &Value, expected: StatusCode) {
    assert_eq!(status, expected, "状态码不符，响应体: {body}");
    assert!(
        body.get("message").and_then(Value::as_str).is_some(),
        "错误响应必须是 {{\"message\": ...}}: {body}"
    );
}

fn usage_record(id: &str, model: &str, provider: &str, created_at: i64) -> UsageRecord {
    UsageRecord {
        id: id.to_string(),
        session_id: Some("sess-1".to_string()),
        model: Some(model.to_string()),
        upstream_model: None,
        provider_key: Some(provider.to_string()),
        input_tokens: 10,
        output_tokens: 20,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        reasoning_tokens: 0,
        cost: 0.5,
        status: Some("ok".to_string()),
        error: None,
        latency_ms: 12,
        ttft_ms: Some(3),
        created_at,
    }
}

// ───────────────────────── 1. 认证中间件 ─────────────────────────

#[tokio::test]
async fn auth_rejects_missing_and_wrong_token() {
    let h = Harness::new("auth");

    let (status, raw) = h
        .raw(Method::GET, "/api/settings", None, None, Vec::new())
        .await;
    assert_error(status, &parse_json(&raw), StatusCode::UNAUTHORIZED);

    let (status, raw) = h
        .raw(
            Method::GET,
            "/api/settings",
            Some("wrong-token"),
            None,
            Vec::new(),
        )
        .await;
    assert_error(status, &parse_json(&raw), StatusCode::UNAUTHORIZED);

    // 正确 token 通过
    let (status, body) = h.get("/api/settings").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.is_array(), "设置列表应为数组: {body}");
}

#[tokio::test]
async fn auth_requires_a_single_bearer_header() {
    let h = Harness::new("auth-strict-bearer");
    for headers in [
        vec![TOKEN.to_string()],
        vec![format!("Basic {TOKEN}")],
        vec![format!("Bearer  {TOKEN}")],
        vec![format!("Bearer {TOKEN} ")],
        vec![format!("Bearer {TOKEN}, Bearer {TOKEN}")],
        vec![format!("Bearer {TOKEN}"), format!("Bearer {TOKEN}")],
        vec![format!("Bearer {TOKEN}"), "Bearer wrong-token".into()],
        vec!["Bearer wrong-token".into(), format!("Bearer {TOKEN}")],
    ] {
        let mut request = Request::builder().uri("/api/config");
        for value in headers {
            request = request.header(header::AUTHORIZATION, value);
        }
        let response = h
            .router
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    for scheme in ["Bearer", "bearer", "BEARER"] {
        let request = Request::builder()
            .uri("/api/config")
            .header(header::AUTHORIZATION, format!("{scheme} {TOKEN}"))
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            h.router.clone().oneshot(request).await.unwrap().status(),
            StatusCode::OK
        );
    }
}

#[tokio::test]
async fn unauthorized_config_and_restart_requests_have_no_side_effects() {
    let mut config = AppConfig::default();
    config.gateway.auth_token = Some("gateway-only-token".into());
    let h = Harness::with_config("auth-mutations", config);
    let before = std::fs::read(&h.paths.config_file).unwrap();
    let (_, mut update) = h.get("/api/config").await;
    update["logLevel"] = json!("error");
    update["gateway"]["authToken"] = json!("injected-gateway-token");
    for token in [None, Some("wrong-token"), Some("gateway-only-token")] {
        let (status, raw) = h
            .raw(
                Method::PUT,
                "/api/config",
                token,
                Some("application/json"),
                update.to_string().into_bytes(),
            )
            .await;
        assert_error(status, &parse_json(&raw), StatusCode::UNAUTHORIZED);
        let (status, raw) = h
            .raw(
                Method::POST,
                "/api/gateway/restart",
                token,
                None,
                Vec::new(),
            )
            .await;
        assert_error(status, &parse_json(&raw), StatusCode::UNAUTHORIZED);
        assert_eq!(std::fs::read(&h.paths.config_file).unwrap(), before);
        assert_eq!(h.config.read().unwrap().log_level, "info");
        assert_eq!(
            h.config.read().unwrap().gateway.auth_token.as_deref(),
            Some("gateway-only-token")
        );
        assert_eq!(
            h.lifecycle.snapshot().phase,
            moonbridge_server::lifecycle::Phase::Running
        );
    }
}

#[tokio::test]
async fn merged_router_keeps_models_public_and_credentials_separate() {
    let mut config = AppConfig::default();
    config.gateway.auth_token = Some("gateway-only-token".into());
    let mut h = Harness::with_config("auth-merged", config);
    let state =
        moonbridge_gateway::bootstrap(h.config.read().unwrap().gateway.clone(), h.db.clone())
            .unwrap();
    h.router = h
        .router
        .clone()
        .merge(moonbridge_gateway::server::router(state));
    for uri in ["/v1/models", "/models"] {
        for token in [None, Some("wrong-token")] {
            let (status, raw) = h.raw(Method::GET, uri, token, None, Vec::new()).await;
            assert_eq!(status, StatusCode::OK, "{raw}");
            assert!(parse_json(&raw)["data"].is_array());
        }
    }
    let (status, raw) = h
        .raw(
            Method::GET,
            "/api/config",
            Some("gateway-only-token"),
            None,
            Vec::new(),
        )
        .await;
    assert_error(status, &parse_json(&raw), StatusCode::UNAUTHORIZED);
    for uri in [
        "/v1/chat/completions",
        "/v1/responses",
        "/responses",
        "/v1/messages",
    ] {
        let request = json!({"model": "missing-model", "messages": [{"role": "user", "content": "hello"}], "input": "hello", "max_tokens": 1});
        let (status, _) = h
            .raw(
                Method::POST,
                uri,
                Some(TOKEN),
                Some("application/json"),
                request.to_string().into_bytes(),
            )
            .await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "管理 token 不应拥有网关权限: {uri}"
        );
    }
    assert_eq!(h.get("/api/config").await.0, StatusCode::OK);
}

#[tokio::test]
async fn auth_guards_unknown_api_paths_before_not_found() {
    let h = Harness::new("auth-unknown");

    // route_layer 覆盖 `/api/*rest` 兜底路由：未认证的未知路径先 401，而不是 404/静态回落
    let (status, raw) = h
        .raw(
            Method::GET,
            "/api/definitely-not-a-route",
            None,
            None,
            Vec::new(),
        )
        .await;
    assert_error(status, &parse_json(&raw), StatusCode::UNAUTHORIZED);

    // 认证通过后才是 JSON 404（而非被 SPA fallback 吞成 200 HTML）
    let (status, body) = h.get("/api/definitely-not-a-route").await;
    assert_error(status, &body, StatusCode::NOT_FOUND);

    for method in [Method::POST, Method::PUT, Method::DELETE] {
        let (status, raw) = h
            .raw(
                method,
                "/api/nope",
                Some(TOKEN),
                Some("application/json"),
                b"null".to_vec(),
            )
            .await;
        assert_error(status, &parse_json(&raw), StatusCode::NOT_FOUND);
    }
}

// ───────────────────────── 2. provider / model / route / settings CRUD ─────────────────────────

#[tokio::test]
async fn provider_crud_round_trip() {
    let h = Harness::new("provider");

    let provider = json!({
        "key": "acme",
        "endpoints": [
            { "protocol": "openai-chat", "baseUrl": "https://api.acme.test/v1", "apiKey": "sk-acme" }
        ],
        "version": null,
        "userAgent": "moonbridge-test",
        "webSearch": null,
        "extra": { "note": "x" },
        "enabled": true,
        "createdAt": 0,
        "updatedAt": 0
    });
    let (status, body) = h
        .json(Method::PUT, "/api/providers", provider.clone())
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, Value::Null, "保存返回 null");

    let (status, body) = h.get("/api/providers/acme").await;
    assert_eq!(status, StatusCode::OK);
    let got: Provider = serde_json::from_value(body).unwrap();
    assert_eq!(got.key, "acme");
    assert_eq!(got.endpoints.len(), 1);
    assert_eq!(got.endpoints[0].base_url, "https://api.acme.test/v1");
    assert_eq!(got.endpoints[0].api_key, "sk-acme");
    assert_eq!(got.user_agent.as_deref(), Some("moonbridge-test"));
    assert!(got.enabled);
    assert!(got.created_at > 0, "落库应补 created_at");
    assert_eq!(got.extra, json!({ "note": "x" }));

    let (status, body) = h.get("/api/providers").await;
    assert_eq!(status, StatusCode::OK);
    let list: Vec<Provider> = serde_json::from_value(body).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].key, "acme");

    let (status, body) = h.delete("/api/providers/acme").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = h.get("/api/providers/acme").await;
    assert_error(status, &body, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn model_and_route_crud_round_trip() {
    let h = Harness::new("model-route");

    let model = json!({
        "slug": "claude-x",
        "displayName": "Claude X",
        "contextWindow": 200000,
        "maxOutputTokens": 8192,
        "modalities": ["text", "image"],
        "reasoningLevels": ["low", "high"],
        "extra": null
    });
    let (status, _) = h.json(Method::PUT, "/api/models", model).await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = h.get("/api/models/claude-x").await;
    assert_eq!(status, StatusCode::OK);
    let got: ModelDef = serde_json::from_value(body).unwrap();
    assert_eq!(got.display_name.as_deref(), Some("Claude X"));
    assert_eq!(got.context_window, Some(200_000));
    assert_eq!(got.modalities, Some(json!(["text", "image"])));
    let (status, body) = h.get("/api/models").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        serde_json::from_value::<Vec<ModelDef>>(body).unwrap().len(),
        1
    );

    // 路由别名
    let route = json!({
        "alias": "fast",
        "modelSlug": "claude-x",
        "providerKey": "acme",
        "extra": null
    });
    let (status, _) = h.json(Method::PUT, "/api/routes", route).await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = h.get("/api/routes/fast").await;
    assert_eq!(status, StatusCode::OK);
    let got: Route = serde_json::from_value(body).unwrap();
    assert_eq!(got.model_slug, "claude-x");
    assert_eq!(got.provider_key, "acme");
    let (status, body) = h.get("/api/routes").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(serde_json::from_value::<Vec<Route>>(body).unwrap().len(), 1);

    // 删除后 404
    let (status, _) = h.delete("/api/models/claude-x").await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = h.get("/api/models/claude-x").await;
    assert_error(status, &body, StatusCode::NOT_FOUND);
    let (status, _) = h.delete("/api/routes/fast").await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = h.get("/api/routes/fast").await;
    assert_error(status, &body, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn settings_crud_round_trip() {
    let h = Harness::new("settings");

    let (status, _) = h
        .json(Method::PUT, "/api/settings/theme", json!({ "dark": true }))
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = h.get("/api/settings/theme").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "dark": true }));

    let (status, body) = h.get("/api/settings").await;
    assert_eq!(status, StatusCode::OK);
    let list: Vec<Setting> = serde_json::from_value(body).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].key, "theme");
    assert_eq!(list[0].value, json!({ "dark": true }));

    // 不存在的键返回 null（对齐桌面端语义，不报错）
    let (status, body) = h.get("/api/settings/missing").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, Value::Null);

    let (status, _) = h.delete("/api/settings/theme").await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = h.get("/api/settings").await;
    assert_eq!(status, StatusCode::OK);
    assert!(serde_json::from_value::<Vec<Setting>>(body)
        .unwrap()
        .is_empty());
}

// ───────────────────────── 3. offers ─────────────────────────

#[tokio::test]
async fn offer_save_list_delete_round_trip() {
    let h = Harness::new("offers");

    let offer = json!({
        "providerKey": "acme",
        "modelSlug": "claude-x",
        "pricing": { "input": 3.0, "output": 15.0 },
        "endpointProtocol": "openai-chat"
    });
    let (status, body) = h.json(Method::PUT, "/api/offers", offer).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, Value::Null);

    let (status, body) = h.get("/api/providers/acme/offers").await;
    assert_eq!(status, StatusCode::OK);
    let list: Vec<Offer> = serde_json::from_value(body).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].model_slug, "claude-x");
    assert_eq!(
        list[0]
            .pricing
            .as_ref()
            .unwrap()
            .get("input")
            .and_then(Value::as_f64),
        Some(3.0)
    );
    assert_eq!(list[0].endpoint_protocol.as_deref(), Some("openai-chat"));

    // 另一个 provider 的报价互不影响
    let (status, body) = h.get("/api/providers/other/offers").await;
    assert_eq!(status, StatusCode::OK);
    assert!(serde_json::from_value::<Vec<Offer>>(body)
        .unwrap()
        .is_empty());

    let (status, body) = h.delete("/api/providers/acme/offers/claude-x").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, body) = h.get("/api/providers/acme/offers").await;
    assert!(serde_json::from_value::<Vec<Offer>>(body)
        .unwrap()
        .is_empty());
}

// ───────────────────────── 4. 用量 ─────────────────────────

#[tokio::test]
async fn usage_query_and_summary() {
    let h = Harness::new("usage");
    h.db.insert_usage(&usage_record("u1", "m-a", "p1", 1_000))
        .unwrap();
    h.db.insert_usage(&usage_record("u2", "m-b", "p2", 2_000))
        .unwrap();
    let mut third = usage_record("u3", "m-a", "p1", 3_000);
    third.input_tokens = 1;
    third.output_tokens = 2;
    third.cost = 1.0;
    h.db.insert_usage(&third).unwrap();

    let (status, body) = h.get("/api/usage").await;
    assert_eq!(status, StatusCode::OK);
    let list: Vec<UsageRecord> = serde_json::from_value(body).unwrap();
    assert_eq!(list.len(), 3);
    assert_eq!(list[0].id, "u3", "按 created_at 倒序");

    let (_, body) = h.get("/api/usage?limit=1").await;
    assert_eq!(
        serde_json::from_value::<Vec<UsageRecord>>(body)
            .unwrap()
            .len(),
        1
    );

    let (_, body) = h.get("/api/usage?model=m-a").await;
    assert_eq!(
        serde_json::from_value::<Vec<UsageRecord>>(body)
            .unwrap()
            .len(),
        2
    );

    let (_, body) = h.get("/api/usage?providerKey=p1").await;
    assert_eq!(
        serde_json::from_value::<Vec<UsageRecord>>(body)
            .unwrap()
            .len(),
        2
    );

    let (_, body) = h.get("/api/usage?status=error").await;
    assert!(serde_json::from_value::<Vec<UsageRecord>>(body)
        .unwrap()
        .is_empty());

    let (status, body) = h.get("/api/usage/summary").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["requests"], json!(3));
    assert_eq!(body["inputTokens"], json!(21));
    assert_eq!(body["outputTokens"], json!(42));
    assert_eq!(body["totalCost"], json!(2.0));
}

// ───────────────────────── 5. trace ─────────────────────────

#[tokio::test]
async fn trace_list_read_delete_and_traversal_is_rejected() {
    let h = Harness::new("trace");

    let model_dir = h.paths.trace_dir.join("sess-1").join("claude-x");
    std::fs::create_dir_all(&model_dir).unwrap();
    std::fs::write(
        model_dir.join("1-abc.json"),
        json!({ "id": "abc" }).to_string(),
    )
    .unwrap();
    std::fs::write(model_dir.join("ignore.txt"), "not json").unwrap();
    // 诱饵：根之外的真实文件，任何穿越尝试都不许读到或删掉它
    let decoy = h.dir.join("secret.json");
    std::fs::write(&decoy, json!({ "secret": true }).to_string()).unwrap();

    let (status, body) = h.get("/api/traces").await;
    assert_eq!(status, StatusCode::OK);
    let list: Vec<Value> = serde_json::from_value(body).unwrap();
    assert_eq!(list.len(), 1, "只列 .json：{list:?}");
    assert_eq!(list[0]["session"], json!("sess-1"));
    assert_eq!(list[0]["model"], json!("claude-x"));
    assert_eq!(list[0]["fileName"], json!("1-abc.json"));
    assert_eq!(list[0]["relPath"], json!("sess-1/claude-x/1-abc.json"));
    assert!(list[0]["size"].as_u64().unwrap() > 0);
    assert!(list[0]["modifiedAt"].as_u64().unwrap() > 0);

    let (status, body) = h.get("/api/traces?limit=1").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(serde_json::from_value::<Vec<Value>>(body).unwrap().len(), 1);

    let (status, body) = h.get("/api/trace?path=sess-1/claude-x/1-abc.json").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "id": "abc" }));

    // 缺少 path 参数 → 400
    let (status, body) = h.get("/api/trace").await;
    assert_error(status, &body, StatusCode::BAD_REQUEST);

    // `../` 上跳 → 4xx，且诱饵文件必须原封不动
    let (status, body) = h
        .raw(
            Method::GET,
            "/api/trace?path=../../secret.json",
            Some(TOKEN),
            None,
            Vec::new(),
        )
        .await;
    assert!(
        status.is_client_error(),
        "上跳必须被 4xx 拒绝，实际 {status}: {body}"
    );

    // 绝对路径穿越 → 4xx
    let (status, body) = h
        .raw(
            Method::GET,
            "/api/trace?path=/etc/passwd",
            Some(TOKEN),
            None,
            Vec::new(),
        )
        .await;
    assert!(
        status.is_client_error(),
        "绝对路径必须被 4xx 拒绝，实际 {status}: {body}"
    );

    // DELETE 同样受穿越校验保护
    let (status, body) = h
        .raw(
            Method::DELETE,
            "/api/trace?path=../../secret.json",
            Some(TOKEN),
            None,
            Vec::new(),
        )
        .await;
    assert!(
        status.is_client_error(),
        "删除的上跳必须被 4xx 拒绝，实际 {status}: {body}"
    );
    assert!(decoy.exists(), "根之外的文件不得被删除");

    // 正常删除
    let (status, body) = h.delete("/api/trace?path=sess-1/claude-x/1-abc.json").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!model_dir.join("1-abc.json").exists());
    let (status, body) = h.get("/api/trace?path=sess-1/claude-x/1-abc.json").await;
    assert_error(status, &body, StatusCode::NOT_FOUND);
}

// ───────────────────────── 6. plugin ─────────────────────────

fn plugin_body(name: &str, script_ref: &str, enabled: bool) -> Value {
    json!({
        "name": name,
        "source": "lua",
        "scriptRef": script_ref,
        "enabled": enabled,
        "config": null,
        "scopes": ["global"],
        "capabilities": ["core"]
    })
}

#[tokio::test]
async fn plugin_enable_is_gated_by_requires() {
    let mut config = AppConfig::default();
    config.gateway.session_marker = false; // 与脚本声明的 sessionMarker = true 不符
    let h = Harness::with_config("plugin-gate", config);

    let script = "MB = { requires = { sessionMarker = true } }\nreturn {}\n";
    let (status, body) = h
        .json(
            Method::PUT,
            "/api/plugins",
            plugin_body("gate", script, true),
        )
        .await;
    assert_error(status, &body, StatusCode::BAD_REQUEST);
    let message = body["message"].as_str().unwrap();
    assert!(
        message.starts_with("REQUIREMENTS"),
        "消息须以 REQUIREMENTS 开头（前端据此弹提示框）: {message}"
    );
    assert!(message.contains("会话水印"), "{message}");
    // 门控拒绝时不应落库
    assert!(
        h.db.get_plugin("gate").unwrap().is_none(),
        "未满足门控的插件不得落库"
    );

    // 停用状态保存则不做门控
    let (status, _) = h
        .json(
            Method::PUT,
            "/api/plugins",
            plugin_body("gate", script, false),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!h.db.get_plugin("gate").unwrap().unwrap().enabled);

    // 满足门控后可以启用
    let h2 = Harness::with_config("plugin-gate-ok", AppConfig::default());
    let (status, body) = h2
        .json(
            Method::PUT,
            "/api/plugins",
            plugin_body("gate", script, true),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(h2.db.get_plugin("gate").unwrap().unwrap().enabled);
}

#[tokio::test]
async fn plugin_script_read_write_inline_and_file() {
    let h = Harness::new("plugin-script");

    // 内联脚本：script_ref 即源码
    let (status, _) = h
        .json(
            Method::PUT,
            "/api/plugins",
            plugin_body("inline", "print('v1')", false),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = h.get_text("/api/plugins/inline/script").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "print('v1')");

    let (status, body) = h
        .raw(
            Method::PUT,
            "/api/plugins/inline/script",
            Some(TOKEN),
            Some("text/plain; charset=utf-8"),
            b"print('v2')".to_vec(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, body) = h.get_text("/api/plugins/inline/script").await;
    assert_eq!(body, "print('v2')", "内联脚本内容应被替换");
    assert_eq!(
        h.db.get_plugin("inline").unwrap().unwrap().script_ref,
        "print('v2')"
    );

    // 文件脚本：写入 plugins_dir 内，并把 script_ref 规范化为绝对路径
    let (status, _) = h
        .json(
            Method::PUT,
            "/api/plugins",
            plugin_body("filer", "filer.lua", false),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = h.get_text("/api/plugins/filer/script").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, "", "文件尚未落盘时返回空串");

    let (status, body) = h
        .raw(
            Method::PUT,
            "/api/plugins/filer/script",
            Some(TOKEN),
            Some("text/plain; charset=utf-8"),
            b"print('from file')".to_vec(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let written = h.paths.plugins_dir.join("filer.lua");
    assert!(written.is_file(), "脚本应写入 plugins_dir 内");
    assert_eq!(
        std::fs::read_to_string(&written).unwrap(),
        "print('from file')"
    );
    let rec = h.db.get_plugin("filer").unwrap().unwrap();
    assert_eq!(
        rec.script_ref,
        written.to_string_lossy(),
        "script_ref 应规范化为绝对路径，供网关按同一路径读取"
    );
    let (_, body) = h.get_text("/api/plugins/filer/script").await;
    assert_eq!(body, "print('from file')");

    // 不存在的插件 → 404
    let (status, body) = h.get("/api/plugins/ghost/script").await;
    assert_error(status, &body, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn plugin_script_ref_outside_plugins_dir_is_rejected() {
    let h = Harness::new("plugin-escape");

    // 直接落库一个越界的 script_ref（绕过保存路径的门控），读取/写入都必须拒绝
    h.db.upsert_plugin(&PluginRecord {
        name: "escape".to_string(),
        source: "lua".to_string(),
        script_ref: "/etc/passwd.lua".to_string(),
        enabled: false,
        config: Value::Null,
        scopes: vec!["global".to_string()],
        capabilities: vec!["core".to_string()],
    })
    .unwrap();

    let (status, body) = h.get("/api/plugins/escape/script").await;
    assert_error(status, &body, StatusCode::BAD_REQUEST);
    assert!(body["message"].as_str().unwrap().contains("越出插件目录"));

    let (status, raw) = h
        .raw(
            Method::PUT,
            "/api/plugins/escape/script",
            Some(TOKEN),
            Some("text/plain; charset=utf-8"),
            b"pwned".to_vec(),
        )
        .await;
    assert_error(status, &parse_json(&raw), StatusCode::BAD_REQUEST);

    // 保存路径同样拦截：启用时解析 script_ref 即拒绝
    let (status, body) = h
        .json(
            Method::PUT,
            "/api/plugins",
            plugin_body("escape2", "/etc/passwd.lua", true),
        )
        .await;
    assert_error(status, &body, StatusCode::BAD_REQUEST);

    // `../` 上跳也被拒
    let (status, body) = h
        .json(
            Method::PUT,
            "/api/plugins",
            plugin_body("escape3", "../../evil.lua", true),
        )
        .await;
    assert_error(status, &body, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn plugin_import_writes_file_and_reports_per_file_status() {
    let h = Harness::new("plugin-import");

    let script = "MB = { scopes = { \"global\", \"provider\" }, capabilities = { \"core\" } }\n";
    let (status, body) = h
        .json(
            Method::POST,
            "/api/plugins/import",
            json!({ "files": [
                { "name": "imported.lua", "content": script },
                { "name": "bad name.lua", "content": "" },
                { "name": "imported.lua", "content": script }
            ]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let outcomes = body.as_array().unwrap();
    assert_eq!(outcomes.len(), 3);
    assert_eq!(outcomes[0]["name"], json!("imported"));
    assert_eq!(outcomes[0]["status"], json!("imported"));
    assert_eq!(outcomes[0]["path"], json!("imported.lua"));
    assert!(outcomes[0]["message"].is_null(), "无未满足项时无 message");
    assert_eq!(outcomes[1]["status"], json!("error"), "非法文件名");
    assert!(outcomes[1]["message"].as_str().unwrap().contains("插件名"));
    assert_eq!(outcomes[2]["status"], json!("skipped"), "同名不覆盖");

    // 落库 + 落盘
    let rec = h.db.get_plugin("imported").unwrap().expect("应已落库");
    assert!(rec.enabled, "无 requires 时导入即启用");
    assert_eq!(rec.scopes, vec!["global", "provider"]);
    assert_eq!(rec.capabilities, vec!["core"]);
    let written = h.paths.plugins_dir.join("imported.lua");
    assert_eq!(std::fs::read_to_string(&written).unwrap(), script);
    // 写入后 script_ref 被规范化为 plugins_dir 内的绝对路径（与在线编辑同一语义）
    assert_eq!(rec.script_ref, written.to_string_lossy());

    // GET /api/plugins 能看到它
    let (_, body) = h.get("/api/plugins").await;
    let list: Vec<PluginRecord> = serde_json::from_value(body).unwrap();
    assert!(list.iter().any(|p| p.name == "imported"));

    // 脚本可读回
    let (status, body) = h.get_text("/api/plugins/imported/script").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, script);
}

#[tokio::test]
async fn plugin_import_with_unmet_requires_stays_disabled() {
    let mut config = AppConfig::default();
    config.gateway.session_marker = false;
    let h = Harness::with_config("plugin-import-gate", config);

    let (status, body) = h
        .json(
            Method::POST,
            "/api/plugins/import",
            json!({ "files": [{
                "name": "gated.lua",
                "content": "MB = { requires = { sessionMarker = true } }\n"
            }]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body[0]["status"], json!("imported"), "不阻断导入");
    let message = body[0]["message"].as_str().unwrap();
    assert!(message.contains("保持停用"), "{message}");
    assert!(!h.db.get_plugin("gated").unwrap().unwrap().enabled);
    assert!(
        h.paths.plugins_dir.join("gated.lua").is_file(),
        "脚本仍应落盘"
    );
}

#[tokio::test]
async fn plugin_delete_then_get_is_not_found() {
    let h = Harness::new("plugin-delete");
    let (status, _) = h
        .json(
            Method::PUT,
            "/api/plugins",
            plugin_body("temp", "print(1)", false),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = h.get("/api/plugins/temp").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["name"], json!("temp"));

    let (status, _) = h.delete("/api/plugins/temp").await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = h.get("/api/plugins/temp").await;
    assert_error(status, &body, StatusCode::NOT_FOUND);
}

// ───────────────────────── 7. catalog 导入 ─────────────────────────

#[tokio::test]
async fn catalog_import_persists_models_and_writes_offers() {
    let h = Harness::new("catalog-import");

    let (status, body) = h
        .json(
            Method::POST,
            "/api/catalog/import",
            json!([{
                "providerKey": "opencode",
                "providerName": "OpenCode Zen",
                "id": "muse-spark",
                "name": "Muse Spark",
                "contextWindow": 1048576,
                "maxOutputTokens": 131072,
                "modalities": ["text", "image"],
                "reasoningLevels": ["high"],
                "pricing": { "input": 1.0, "output": 2.0 }
            }]),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["imported"], json!(1));

    let (status, body) = h.get("/api/models/muse-spark").await;
    assert_eq!(status, StatusCode::OK);
    let model: ModelDef = serde_json::from_value(body).unwrap();
    assert_eq!(model.display_name.as_deref(), Some("Muse Spark"));
    assert_eq!(model.context_window, Some(1_048_576));
    assert_eq!(model.max_output_tokens, Some(131_072));
    assert_eq!(model.modalities, Some(json!(["text", "image"])));

    let (status, body) = h.get("/api/providers/opencode/offers").await;
    assert_eq!(status, StatusCode::OK);
    let offers: Vec<Offer> = serde_json::from_value(body).unwrap();
    assert_eq!(offers.len(), 1);
    let pricing = offers[0].pricing.clone().unwrap();
    assert_eq!(pricing.get("input").and_then(Value::as_f64), Some(1.0));
    assert_eq!(pricing.get("output").and_then(Value::as_f64), Some(2.0));
}

/// 导入时定价回填只填 `pricing IS NULL` 的行（移植桌面端
/// `backfill_only_fills_null_pricing` 的语义：用户手填过的定价不得被目录覆盖）。
#[tokio::test]
async fn catalog_import_backfills_only_null_pricing() {
    let h = Harness::new("catalog-backfill");

    // 注意顺序：upsert_offer 对 pricing=None 的新行会继承同 slug 的已有定价，
    // 空行须先于手填行插入才能保持 NULL。
    for (key, pricing) in [("mykey", None), ("mykey2", Some(json!({ "input": 99.0 })))] {
        h.db.upsert_offer(&Offer {
            provider_key: key.to_string(),
            model_slug: "claude-x".to_string(),
            pricing,
            endpoint_protocol: None,
        })
        .unwrap();
    }

    let catalog = json!([{
        "providerKey": "anthropic",
        "providerName": "Anthropic",
        "id": "claude-x",
        "name": "Claude X",
        "modalities": [],
        "reasoningLevels": [],
        "pricing": { "input": 3.0, "output": 15.0 }
    }]);
    let (status, body) = h.json(Method::POST, "/api/catalog/import", catalog).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["imported"], json!(1));

    let pricing_of = |key: &str| {
        h.db.list_offers(key)
            .unwrap()
            .into_iter()
            .find(|o| o.model_slug == "claude-x")
            .unwrap()
            .pricing
    };
    assert_eq!(
        pricing_of("mykey"),
        Some(json!({ "input": 3.0, "output": 15.0 })),
        "空定价行应被回填"
    );
    assert_eq!(
        pricing_of("mykey2").and_then(|v| v.get("input").and_then(Value::as_f64)),
        Some(99.0),
        "手填定价不得被覆盖"
    );
    assert_eq!(
        pricing_of("anthropic"),
        Some(json!({ "input": 3.0, "output": 15.0 })),
        "目录 key 行同样写入定价"
    );

    // 再次导入不同定价：已非空的行不再被改写（回填幂等）
    let (status, _) = h
        .json(
            Method::POST,
            "/api/catalog/import",
            json!([{
                "providerKey": "anthropic",
                "providerName": "Anthropic",
                "id": "claude-x",
                "name": "Claude X",
                "modalities": [],
                "reasoningLevels": [],
                "pricing": { "input": 7.0, "output": 7.0 }
            }]),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        pricing_of("mykey").and_then(|v| v.get("input").and_then(Value::as_f64)),
        Some(3.0),
        "非空定价不参与回填"
    );
    assert_eq!(
        pricing_of("anthropic").and_then(|v| v.get("input").and_then(Value::as_f64)),
        Some(3.0),
        "目录 key 已有定价时 insert_offer_if_absent 不覆盖"
    );
}

// ───────────────────────── 8. app info / config / gateway status ─────────────────────────

#[tokio::test]
async fn app_info_reports_server_mode_and_paths() {
    let h = Harness::new("app-info");
    let (status, body) = h.get("/api/app/info").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["mode"], json!("server"));
    assert_eq!(body["version"], json!(env!("CARGO_PKG_VERSION")));
    assert_eq!(body["dbPath"], json!(h.paths.db_path.to_string_lossy()));
    assert_eq!(
        body["configPath"],
        json!(h.paths.config_file.to_string_lossy())
    );
    assert_eq!(body["dataDir"], json!(h.paths.data_dir.to_string_lossy()));
    assert_eq!(
        body["pluginsDir"],
        json!(h.paths.plugins_dir.to_string_lossy())
    );
    assert_eq!(body["traceDir"], json!(h.paths.trace_dir.to_string_lossy()));
}

#[tokio::test]
async fn gateway_status_reports_listener_not_pending_config() {
    let h = Harness::new("gateway-status");
    let (status, body) = h.get("/api/gateway/status").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["running"], json!(true));
    assert_eq!(body["phase"], json!("running"));
    assert_eq!(body["addr"], json!("127.0.0.1:43210"));
    assert_eq!(body["error"], Value::Null);
    let mut config = h.config.read().unwrap().clone();
    config.gateway.addr = "0.0.0.0:39999".to_string();
    let (status, _) = h
        .json(
            Method::PUT,
            "/api/config",
            serde_json::to_value(&config).unwrap(),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = h.get("/api/gateway/status").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["running"], json!(true));
    assert_eq!(body["addr"], json!("127.0.0.1:43210"));
    assert_eq!(h.config.read().unwrap().gateway.addr, "0.0.0.0:39999");
}

#[tokio::test]
async fn gateway_restart_coalesces_and_does_not_override_stop() {
    let h = Harness::new("gateway-restart");
    for _ in 0..2 {
        let (status, body) = h
            .json(Method::POST, "/api/gateway/restart", Value::Null)
            .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(body["message"], json!("restarting"));
    }
    let (_, body) = h.get("/api/gateway/status").await;
    assert_eq!(body["phase"], json!("restarting"));
    assert_eq!(body["running"], json!(false));
    h.lifecycle.request_stop();
    let (status, _) = h
        .json(Method::POST, "/api/gateway/restart", Value::Null)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let (_, body) = h.get("/api/gateway/status").await;
    assert_eq!(body["phase"], json!("stopping"));
    assert_eq!(body["running"], json!(false));
    assert!(!h.lifecycle.begin_start());
}

#[tokio::test]
async fn config_get_is_redacted_and_round_trip_preserves_separate_tokens() {
    let mut runtime = AppConfig::default();
    runtime.gateway.auth_token = Some("environment-gateway-token".into());
    let mut persisted = AppConfig::default();
    persisted.gateway.auth_token = Some("persisted-gateway-token".into());
    let h = Harness::with_configs("config-redacted-roundtrip", runtime, persisted);
    for token_value in [None, Some(Value::Null), Some(json!("")), Some(json!("   "))] {
        let (status, mut body) = h.get("/api/config").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["gateway"].get("authToken"), Some(&Value::Null));
        let text = body.to_string();
        assert!(!text.contains(TOKEN));
        assert!(!text.contains("environment-gateway-token"));
        assert!(!text.contains("persisted-gateway-token"));
        body["logLevel"] = json!("warn");
        match token_value {
            Some(value) => body["gateway"]["authToken"] = value,
            None => {
                body["gateway"].as_object_mut().unwrap().remove("authToken");
            }
        }
        let (status, response) = h.json(Method::PUT, "/api/config", body).await;
        assert_eq!(status, StatusCode::OK, "{response}");
        assert_eq!(response, Value::Null);
        assert_eq!(
            h.config.read().unwrap().gateway.auth_token.as_deref(),
            Some("environment-gateway-token")
        );
        assert_eq!(
            h.persisted_config
                .read()
                .unwrap()
                .gateway
                .auth_token
                .as_deref(),
            Some("persisted-gateway-token")
        );
        let reloaded = AppConfig::load_or_default(&h.paths.config_file).unwrap();
        assert_eq!(reloaded.log_level, "warn");
        assert_eq!(
            reloaded.gateway.auth_token.as_deref(),
            Some("persisted-gateway-token")
        );
        let text = std::fs::read_to_string(&h.paths.config_file).unwrap();
        assert!(!text.contains(TOKEN));
        assert!(!text.contains("environment-gateway-token"));
    }
}

#[tokio::test]
async fn unrelated_config_put_does_not_persist_environment_gateway_token() {
    let mut runtime = AppConfig::default();
    runtime.gateway.auth_token = Some("environment-only-token".into());
    let h = Harness::with_configs("config-environment-only", runtime, AppConfig::default());
    let (_, mut body) = h.get("/api/config").await;
    body["logLevel"] = json!("debug");
    let (status, _) = h.json(Method::PUT, "/api/config", body).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        h.config.read().unwrap().gateway.auth_token.as_deref(),
        Some("environment-only-token")
    );
    assert!(AppConfig::load_or_default(&h.paths.config_file)
        .unwrap()
        .gateway
        .auth_token
        .is_none());
    let text = std::fs::read_to_string(&h.paths.config_file).unwrap();
    assert!(!text.contains("environment-only-token"));
    assert!(!text.contains(TOKEN));
}

#[tokio::test]
async fn config_put_can_explicitly_rotate_gateway_token_but_not_admin_token() {
    let h = Harness::new("config-put");
    let (_, mut body) = h.get("/api/config").await;
    body["logLevel"] = json!("warn");
    body["gateway"]["addr"] = json!("127.0.0.1:41234");
    body["gateway"]["authToken"] = json!(" rotated-gateway-token ");
    let (status, response) = h.json(Method::PUT, "/api/config", body).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response, Value::Null);
    let reloaded = AppConfig::load_or_default(&h.paths.config_file).unwrap();
    assert_eq!(reloaded.log_level, "warn");
    assert_eq!(reloaded.gateway.addr, "127.0.0.1:41234");
    assert_eq!(
        reloaded.gateway.auth_token.as_deref(),
        Some("rotated-gateway-token")
    );
    assert_eq!(
        h.config.read().unwrap().gateway.auth_token.as_deref(),
        Some("rotated-gateway-token")
    );
    let (status, body) = h.get("/api/config").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["gateway"].get("authToken"), Some(&Value::Null));
    assert_eq!(body["logLevel"], json!("warn"));
    assert_eq!(body["gateway"]["addr"], json!("127.0.0.1:41234"));
    let before = std::fs::read(&h.paths.config_file).unwrap();
    for token in [TOKEN.to_string(), format!(" {TOKEN} ")] {
        let mut rejected = body.clone();
        rejected["gateway"]["authToken"] = json!(token);
        let (status, response) = h.json(Method::PUT, "/api/config", rejected).await;
        assert_error(status, &response, StatusCode::BAD_REQUEST);
        assert!(!response.to_string().contains(TOKEN));
        assert_eq!(std::fs::read(&h.paths.config_file).unwrap(), before);
        assert_eq!(
            h.config.read().unwrap().gateway.auth_token.as_deref(),
            Some("rotated-gateway-token")
        );
        assert_eq!(
            h.persisted_config
                .read()
                .unwrap()
                .gateway
                .auth_token
                .as_deref(),
            Some("rotated-gateway-token")
        );
    }
    let (status, _) = h
        .raw(
            Method::GET,
            "/api/config",
            Some("rotated-gateway-token"),
            None,
            Vec::new(),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(h.get("/api/config").await.0, StatusCode::OK);
}

#[tokio::test]
async fn invalid_gateway_tokens_reject_the_entire_config_update() {
    let mut runtime = AppConfig::default();
    runtime.gateway.auth_token = Some("runtime-private-token".into());
    let mut persisted = AppConfig::default();
    persisted.gateway.auth_token = Some("persisted-private-token".into());
    let h = Harness::with_configs("config-invalid-token", runtime, persisted);
    let runtime_before = serde_json::to_value(&*h.config.read().unwrap()).unwrap();
    let persisted_before = serde_json::to_value(&*h.persisted_config.read().unwrap()).unwrap();
    let disk_before = std::fs::read(&h.paths.config_file).unwrap();
    let (status, original) = h.get("/api/config").await;
    assert_eq!(status, StatusCode::OK);
    for token in [
        "private secret",
        "private\tsecret",
        "private\nsecret",
        "private\rsecret",
        "private\0secret",
        "private\u{1f}secret",
        "private\u{7f}secret",
        "private令牌secret",
        "private\u{a0}secret",
    ] {
        let mut update = original.clone();
        update["gateway"]["authToken"] = json!(token);
        update["gateway"]["addr"] = json!("127.0.0.1:41234");
        update["logLevel"] = json!("error");
        let (status, response) = h.json(Method::PUT, "/api/config", update).await;
        assert_error(status, &response, StatusCode::BAD_REQUEST);
        let response = response.to_string();
        assert!(!response.contains("private"));
        assert!(!response.contains("secret"));
        assert!(!response.contains(TOKEN));
        assert_eq!(
            serde_json::to_value(&*h.config.read().unwrap()).unwrap(),
            runtime_before
        );
        assert_eq!(
            serde_json::to_value(&*h.persisted_config.read().unwrap()).unwrap(),
            persisted_before
        );
        assert_eq!(std::fs::read(&h.paths.config_file).unwrap(), disk_before);
        let (status, current) = h.get("/api/config").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(current, original);
    }
}

#[tokio::test]
async fn failed_config_write_does_not_change_runtime_or_persisted_state() {
    let mut config = AppConfig::default();
    config.gateway.auth_token = Some("original-gateway-token".into());
    let h = Harness::with_config("config-write-failure", config);
    let (_, mut body) = h.get("/api/config").await;
    body["gateway"]["authToken"] = json!("uncommitted-gateway-token");
    body["logLevel"] = json!("error");
    std::fs::remove_file(&h.paths.config_file).unwrap();
    std::fs::create_dir(&h.paths.config_file).unwrap();
    let (status, response) = h.json(Method::PUT, "/api/config", body).await;
    assert_error(status, &response, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(!response.to_string().contains("uncommitted-gateway-token"));
    for config in [&h.config, &h.persisted_config] {
        let config = config.read().unwrap();
        assert_eq!(
            config.gateway.auth_token.as_deref(),
            Some("original-gateway-token")
        );
        assert_eq!(config.log_level, "info");
    }
}

// ───────────────────────── 余额&健康看板 ─────────────────────────

/// 不联网的查询脚本：直接返回固定 table（含中文与 snake_case quotas）。
const BALANCE_SCRIPT: &str = r#"
MB = {}
function MB.query(ctx)
  return {
    status = "ok",
    quotas = { { label = "每周窗口", used_percent = 43, reset_at = "t" } },
    summary = "余额正常",
  }
end
"#;

fn balance_card(key: &str, interval_secs: i64) -> Value {
    json!({
        "key": key,
        "apiKey": "sk-test",
        "baseUrl": "https://api.example.test",
        "providerLabel": "示例服务商",
        "scriptRef": BALANCE_SCRIPT,
        "intervalSecs": interval_secs,
        "enabled": true,
        "extra": {"widget": "quota"},
        "position": 0,
    })
}

#[tokio::test]
async fn balance_cards_crud_roundtrip() {
    let h = Harness::new("balance-crud");

    let (status, body) = h.get("/api/balance/cards").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!([]), "初始为空数组: {body}");

    // 保存（intervalSecs=5 应被夹到 60）
    let (status, body) = h
        .json(Method::PUT, "/api/balance/cards", balance_card("main", 5))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = h.get("/api/balance/cards").await;
    assert_eq!(status, StatusCode::OK);
    let cards = body.as_array().expect("列表应为数组").clone();
    assert_eq!(cards.len(), 1, "{body}");
    assert_eq!(cards[0]["key"], json!("main"));
    assert_eq!(cards[0]["apiKey"], json!("sk-test"));
    assert_eq!(cards[0]["providerLabel"], json!("示例服务商"));
    assert_eq!(cards[0]["intervalSecs"], json!(60), "保存时 1..=59 夹到 60");
    assert_eq!(cards[0]["extra"]["widget"], json!("quota"));
    assert_eq!(
        cards[0]["results"],
        json!([]),
        "从未查询时 results 为空数组"
    );

    // 冲突更新不新增行
    let (status, _) = h
        .json(
            Method::PUT,
            "/api/balance/cards",
            balance_card("main", 3600),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (_, body) = h.get("/api/balance/cards").await;
    let cards = body.as_array().unwrap();
    assert_eq!(cards.len(), 1, "upsert 不得新增行: {body}");
    assert_eq!(cards[0]["intervalSecs"], json!(3600));

    // 删除后列表为空
    let (status, _) = h.delete("/api/balance/cards/main").await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = h.get("/api/balance/cards").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!([]), "删除后列表应回到空: {body}");
}

#[tokio::test]
async fn balance_endpoints_require_auth() {
    let h = Harness::new("balance-auth");

    for (method, uri) in [
        (Method::GET, "/api/balance/cards"),
        (Method::PUT, "/api/balance/cards"),
        (Method::DELETE, "/api/balance/cards/main"),
        (Method::POST, "/api/balance/cards/main/refresh"),
        (Method::POST, "/api/balance/refresh"),
    ] {
        let (status, raw) = h.raw(method.clone(), uri, None, None, Vec::new()).await;
        assert_error(status, &parse_json(&raw), StatusCode::UNAUTHORIZED);
    }
}

#[tokio::test]
async fn balance_refresh_runs_inline_script() {
    let h = Harness::new("balance-refresh");

    let (status, _) = h
        .json(Method::PUT, "/api/balance/cards", balance_card("main", 0))
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = h
        .json(Method::POST, "/api/balance/cards/main/refresh", Value::Null)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["key"], json!("main"));
    let results = body["results"].as_array().expect("results 应为数组");
    assert_eq!(results.len(), 1, "手填单 key 卡片拆一行: {body}");
    let result = &results[0];
    assert_eq!(result["keyIndex"], json!(0));
    assert_eq!(result["keyLabel"], json!("sk…"), "label 是掩码而非原文");
    assert_eq!(result["status"], json!("ok"));
    assert_eq!(result["error"], Value::Null);
    let quota = &result["payload"]["quotas"][0];
    assert_eq!(quota["label"], json!("每周窗口"));
    assert_eq!(
        quota["usedPercent"],
        json!(43.0),
        "归一为 camelCase: {quota}"
    );
    assert_eq!(quota["leftPercent"], json!(57.0), "互补值应补齐");
    assert_eq!(quota["resetAt"], json!("t"));
    assert_eq!(result["payload"]["summary"], json!("余额正常"));

    // 单 key 刷新（query key_index=0）：同样返回整卡视图
    let (status, body) = h
        .json(
            Method::POST,
            "/api/balance/cards/main/refresh?key_index=0",
            Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["results"][0]["status"], json!("ok"));

    let (status, body) = h.get("/api/balance/cards").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body[0]["results"][0]["payload"]["summary"],
        json!("余额正常"),
        "刷新结果应落库"
    );
}

#[tokio::test]
async fn balance_manual_keys_override_provider_via_api() {
    let h = Harness::new("balance-manual-keys");
    let (status, _) = h.json(Method::PUT, "/api/providers", json!({
        "key": "upstream",
        "endpoints": [{"protocol": "openai-chat", "baseUrl": "https://unused.test", "apiKey": "provider-key"}]
    })).await;
    assert_eq!(status, StatusCode::OK);
    let mut card = balance_card("manual", 0);
    card["providerKey"] = json!("upstream");
    card["apiKey"] = json!(" manual-a\r\nmanual-b\nmanual-a\n");
    card["scriptRef"] = json!(
        r#"
    MB = {}
    function MB.query(ctx)
      assert(#ctx.keys == 1 and ctx.keys[1] == ctx.key)
      return { status = "ok", summary = ctx.key }
    end
    "#
    );
    let (status, preview) = h
        .json(Method::POST, "/api/balance/test", card.clone())
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(preview.as_array().unwrap().len(), 2);
    assert_eq!(preview[0]["payload"]["summary"], json!("manual-a"));
    assert_eq!(preview[1]["payload"]["summary"], json!("manual-b"));
    let (_, cards) = h.get("/api/balance/cards").await;
    assert_eq!(cards, json!([]));

    let (status, _) = h
        .json(Method::PUT, "/api/balance/cards", card.clone())
        .await;
    assert_eq!(status, StatusCode::OK);
    let (_, cards) = h.get("/api/balance/cards").await;
    assert_eq!(cards[0]["apiKey"], card["apiKey"]);
    let (status, refreshed) = h
        .json(
            Method::POST,
            "/api/balance/cards/manual/refresh",
            Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(refreshed["results"].as_array().unwrap().len(), 2);
    for idx in 0..2 {
        assert_eq!(refreshed["results"][idx]["status"], json!("ok"));
        assert_eq!(
            refreshed["results"][idx]["payload"],
            preview[idx]["payload"]
        );
    }

    card["providerKey"] = json!("missing");
    let (status, preview) = h
        .json(Method::POST, "/api/balance/test", card.clone())
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(preview.as_array().unwrap().len(), 2);
    assert_eq!(preview[0]["status"], json!("ok"));
    assert_eq!(preview[1]["status"], json!("ok"));

    card["providerKey"] = json!("upstream");
    card["apiKey"] = json!(" \r\n ");
    let (status, _) = h.json(Method::PUT, "/api/balance/cards", card).await;
    assert_eq!(status, StatusCode::OK);
    let (status, refreshed) = h
        .json(
            Method::POST,
            "/api/balance/cards/manual/refresh",
            Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(refreshed["results"].as_array().unwrap().len(), 1);
    assert_eq!(
        refreshed["results"][0]["payload"]["summary"],
        json!("provider-key")
    );
}

#[tokio::test]
async fn balance_refresh_unknown_key_is_404() {
    let h = Harness::new("balance-404");

    let (status, body) = h
        .json(
            Method::POST,
            "/api/balance/cards/ghost/refresh",
            Value::Null,
        )
        .await;
    assert_error(status, &body, StatusCode::NOT_FOUND);

    let (status, body) = h.delete("/api/balance/cards/ghost").await;
    assert_error(status, &body, StatusCode::NOT_FOUND);
}
