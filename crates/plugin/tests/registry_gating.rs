//! LuaPluginRegistry provider 维度三态过滤单测。
//!
//! 覆盖：禁用覆盖、强制启用（全局停用 + binding 启用）、无 binding 跟随全局、
//! 客户端阶段（provider 未知）按全局开关执行。

use std::sync::Arc;

use async_trait::async_trait;
use moonbridge_core::{CoreRequest, CoreResponse};
use moonbridge_plugin::{
    HostBridge, HttpRequest, HttpResponse, LuaPluginRegistry, LuaRuntime, SandboxLimits,
    ScopeOverrides, SessionStore,
};
use moonbridge_protocol::{PluginHooks, RawBody, RawMessage, RawStage, RawVerdict, ReqCtx};

struct DummyBridge;

#[async_trait]
impl HostBridge for DummyBridge {
    async fn http_request(&self, _req: HttpRequest) -> Result<HttpResponse, String> {
        Err("test bridge: http_request 未实现".into())
    }
    async fn provider_invoke(
        &self,
        _provider: &str,
        _model: &str,
        _req: CoreRequest,
    ) -> Result<CoreResponse, String> {
        Err("test bridge: provider_invoke 未实现".into())
    }
}

#[tokio::test]
async fn runtime_hides_debug_and_rejects_sethook() {
    let runtime = LuaRuntime::new(
        "debug-probe",
        r#"
        assert(debug == nil)
        assert(_G.debug == nil)
        assert(load == nil)
        assert(loadstring == nil)
        assert(not pcall(function() debug.sethook() end))
        MB = {}
        function MB.query(ctx)
            assert(debug == nil)
            assert(_G.debug == nil)
            assert(load == nil)
            assert(loadstring == nil)
            local ok = pcall(function() debug.sethook() end)
            return {
                debug_missing = debug == nil,
                sethook_available = ok,
                load_missing = load == nil,
                loadstring_missing = loadstring == nil,
            }
        end
        "#,
        &serde_json::json!({}),
        Arc::new(DummyBridge),
        SessionStore::new(),
    )
    .unwrap();
    let result = runtime
        .call_mb_once("query", &serde_json::json!({}))
        .await
        .unwrap();
    assert_eq!(result["debug_missing"], true);
    assert_eq!(result["sethook_available"], false);
}

#[tokio::test]
async fn debug_cannot_disable_runtime_budget() {
    const CHILD: &str = "MOONBRIDGE_DEBUG_BUDGET_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "debug_cannot_disable_runtime_budget",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success(), "budget regression child failed: {status}");
                return;
            }
            if std::time::Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("Lua loop escaped its execution budget");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    let limits = SandboxLimits {
        max_instructions: 10_000,
        instruction_step: 100,
        call_timeout: std::time::Duration::from_secs(5),
        ..SandboxLimits::default()
    };
    let load_error = LuaRuntime::new_with_limits(
        "top-level-loop",
        "assert(debug == nil); pcall(function() debug.sethook() end); while true do end",
        &serde_json::json!({}),
        Arc::new(DummyBridge),
        SessionStore::new(),
        limits.clone(),
    )
    .err()
    .expect("top-level loop must exhaust its budget");
    assert!(
        load_error.to_string().contains("指令数超过上限"),
        "{load_error}"
    );

    for body in [
        "while true do end",
        "coroutine.wrap(function() while true do end end)()",
    ] {
        let script = format!(
            "MB = {{}}; function MB.query(ctx) assert(debug == nil); \
             pcall(function() debug.sethook() end); {body} end"
        );
        let runtime = LuaRuntime::new_with_limits(
            "hook-loop",
            &script,
            &serde_json::json!({}),
            Arc::new(DummyBridge),
            SessionStore::new(),
            limits.clone(),
        )
        .unwrap();
        let error = runtime
            .call_mb_once("query", &serde_json::json!({}))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("指令数超过上限"), "{error}");
    }
}

const PROBE_SCRIPT: &str = r#"
MB = {
  version = "0.1.0",
  capabilities = { "core" },
}

function MB.on_request(ctx, req)
  req.temperature = 0.5
end
"#;

fn probe_runtime(enabled: bool) -> Arc<LuaRuntime> {
    let mut rt = LuaRuntime::new_with_limits(
        "probe",
        PROBE_SCRIPT,
        &serde_json::json!({}),
        Arc::new(DummyBridge),
        moonbridge_plugin::session::SessionStore::new(),
        SandboxLimits::default(),
    )
    .expect("插件脚本应能加载");
    rt.enabled = enabled;
    Arc::new(rt)
}

fn ctx_for(provider: Option<&str>) -> ReqCtx {
    let ctx = ReqCtx::new("req-1", moonbridge_core::Protocol::Anthropic);
    match provider {
        Some(pk) => ctx.with_route(moonbridge_core::Protocol::Anthropic, pk),
        None => ctx,
    }
}

fn provider_overrides(
    bindings: &[(&str, bool)],
) -> std::collections::HashMap<String, ScopeOverrides> {
    let mut t = ScopeOverrides::default();
    for (key, enabled) in bindings {
        t.insert("provider", key.to_string(), *enabled);
    }
    let mut m = std::collections::HashMap::new();
    m.insert("probe".to_string(), t);
    m
}

#[tokio::test]
async fn provider_binding_overrides_global() {
    let overrides = provider_overrides(&[("off-provider", false), ("on-provider", true)]);
    let registry =
        LuaPluginRegistry::new(vec![probe_runtime(true)], overrides, SessionStore::new());

    // binding 禁用 → 覆盖全局启用
    let mut req = CoreRequest::new("m");
    req.temperature = Some(1.0);
    registry
        .on_request(&ctx_for(Some("off-provider")), &mut req)
        .await
        .unwrap();
    assert_eq!(req.temperature, Some(1.0), "provider 级禁用应生效");

    // binding 启用 → 与全局一致，钩子执行
    let mut req = CoreRequest::new("m");
    registry
        .on_request(&ctx_for(Some("on-provider")), &mut req)
        .await
        .unwrap();
    assert_eq!(req.temperature, Some(0.5));
}

#[tokio::test]
async fn no_binding_follows_global() {
    let registry = LuaPluginRegistry::new(
        vec![probe_runtime(true)],
        Default::default(),
        SessionStore::new(),
    );

    // 无 binding：跟随全局启用
    let mut req = CoreRequest::new("m");
    registry
        .on_request(&ctx_for(Some("any-provider")), &mut req)
        .await
        .unwrap();
    assert_eq!(req.temperature, Some(0.5));
}

#[tokio::test]
async fn force_enable_disabled_plugin() {
    let overrides = provider_overrides(&[("p", true)]);
    let registry =
        LuaPluginRegistry::new(vec![probe_runtime(false)], overrides, SessionStore::new());

    // 全局停用 + provider 强制启用 → 执行
    let mut req = CoreRequest::new("m");
    registry
        .on_request(&ctx_for(Some("p")), &mut req)
        .await
        .unwrap();
    assert_eq!(req.temperature, Some(0.5));

    // 全局停用 + 其它 provider（无 binding）→ 不执行
    let mut req = CoreRequest::new("m");
    registry
        .on_request(&ctx_for(Some("other")), &mut req)
        .await
        .unwrap();
    assert_eq!(req.temperature, None);
}

#[tokio::test]
async fn client_stage_uses_global_only() {
    let overrides = provider_overrides(&[("off-provider", false)]);
    let registry =
        LuaPluginRegistry::new(vec![probe_runtime(true)], overrides, SessionStore::new());

    // 路由前（provider 未知）按全局开关执行，不受 provider binding 影响
    let mut req = CoreRequest::new("m");
    registry.on_request(&ctx_for(None), &mut req).await.unwrap();
    assert_eq!(req.temperature, Some(0.5));
}

/// 就近覆盖顺序：route > model > provider > global > 插件全局开关。
/// 回归：历史上只装配了 provider 维度，model/route/global 绑定写入后不生效。
#[tokio::test]
async fn nearest_scope_wins() {
    let mut t = ScopeOverrides::default();
    t.insert("route", "rt-x".to_string(), false);
    t.insert("model", "m-up".to_string(), true);
    t.insert("provider", "p".to_string(), false);
    t.insert("global", String::new(), true);
    let mut overrides = std::collections::HashMap::new();
    overrides.insert("probe".to_string(), t);
    let registry =
        LuaPluginRegistry::new(vec![probe_runtime(true)], overrides, SessionStore::new());

    let mut ctx = ctx_for(Some("p"));
    ctx.model_alias = "rt-x".to_string();
    ctx.route_alias = Some("rt-x".to_string());
    ctx.upstream_model = Some("m-up".to_string());

    // route 命中 → 禁用（就近优先于 model/provider/global）
    let mut req = CoreRequest::new("rt-x");
    registry.on_request(&ctx, &mut req).await.unwrap();
    assert_eq!(req.temperature, None, "route 维度应就近胜出");

    // 无 route 命中 → model 维度（启用，覆盖 provider=false）
    ctx.route_alias = None;
    let mut req = CoreRequest::new("m-up");
    registry.on_request(&ctx, &mut req).await.unwrap();
    assert_eq!(req.temperature, Some(0.5), "model 维度应覆盖 provider");

    // route/model 都不命中 → provider=false 覆盖 global=true
    ctx.upstream_model = Some("other-model".to_string());
    let mut req = CoreRequest::new("m");
    registry.on_request(&ctx, &mut req).await.unwrap();
    assert_eq!(req.temperature, None, "provider 维度应覆盖 global");

    // 全部不命中 → global
    ctx.provider_key = Some("other".to_string());
    let mut req = CoreRequest::new("m");
    registry.on_request(&ctx, &mut req).await.unwrap();
    assert_eq!(req.temperature, Some(0.5), "global 维度应回退");
}

fn raw_runtime(name: &str, script: &str) -> Arc<LuaRuntime> {
    let mut rt = LuaRuntime::new_with_limits(
        name,
        script,
        &serde_json::json!({}),
        Arc::new(DummyBridge),
        moonbridge_plugin::session::SessionStore::new(),
        SandboxLimits::default(),
    )
    .expect("插件脚本应能加载");
    rt.enabled = true;
    Arc::new(rt)
}

fn raw_msg(stage: RawStage, status: Option<u16>, error: bool) -> RawMessage {
    RawMessage {
        stage,
        protocol: moonbridge_core::Protocol::Anthropic,
        provider: None,
        method: Some("POST".into()),
        url: Some("http://localhost/v1/x".into()),
        status,
        headers: vec![],
        body: RawBody::Empty,
        session_id: None,
        error,
    }
}

fn has_xb(m: &RawMessage) -> bool {
    m.headers.iter().any(|(k, v)| k == "x-b" && v == "1")
}

/// `{action="retry"}` 不得截断同阶段插件扇出：只有「上游响应钩子的错误路径」
/// （msg.error == true）把它当终局判定；其余三个阶段与成功路径一律按放行处理——
/// 后续插件照常执行，它们的就地改写不得丢失。
/// 回归：旧实现所有钩子都是 `Ok(v) => return Ok(v)`，首个返回 retry 的插件
/// 会静默吞掉同阶段其它插件的全部改写。
#[tokio::test]
async fn retry_verdict_does_not_truncate_same_stage_fanout() {
    const RETRY_ALL: &str = r#"
MB = { version = "0.1.0", capabilities = { "raw_request", "raw_response" } }
function MB.on_client_request_raw(ctx, msg) return { action = "retry" } end
function MB.on_upstream_request_raw(ctx, msg) return { action = "retry" } end
function MB.on_upstream_response_raw(ctx, msg) return { action = "retry" } end
function MB.on_client_response_raw(ctx, msg) return { action = "retry" } end
"#;
    const MARK_XB: &str = r#"
MB = { version = "0.1.0", capabilities = { "raw_request", "raw_response" } }
local function mark(ctx, msg)
  msg.headers[#msg.headers + 1] = { "x-b", "1" }
end
function MB.on_client_request_raw(ctx, msg) mark(ctx, msg) end
function MB.on_upstream_request_raw(ctx, msg) mark(ctx, msg) end
function MB.on_upstream_response_raw(ctx, msg) mark(ctx, msg) end
function MB.on_client_response_raw(ctx, msg) mark(ctx, msg) end
"#;
    let registry = LuaPluginRegistry::new(
        vec![
            raw_runtime("a-retry", RETRY_ALL),
            raw_runtime("b-mark", MARK_XB),
        ],
        Default::default(),
        SessionStore::new(),
    );
    let ctx = ctx_for(None);

    // 三个阶段：retry 按放行扇出——verdict 为 Pass 且 B 插件的改写生效
    let mut m = raw_msg(RawStage::ClientRequest, None, false);
    let v = registry.on_client_request_raw(&ctx, &mut m).await.unwrap();
    assert!(
        matches!(v, RawVerdict::Pass),
        "client_request 应放行: {v:?}"
    );
    assert!(has_xb(&m), "B 插件的 header 改写不得被吞");

    let mut m = raw_msg(RawStage::UpstreamRequest, None, false);
    let v = registry
        .on_upstream_request_raw(&ctx, &mut m)
        .await
        .unwrap();
    assert!(
        matches!(v, RawVerdict::Pass),
        "upstream_request 应放行: {v:?}"
    );
    assert!(has_xb(&m), "B 插件的 header 改写不得被吞");

    let mut m = raw_msg(RawStage::ClientResponse, Some(200), false);
    let v = registry.on_client_response_raw(&ctx, &mut m).await.unwrap();
    assert!(
        matches!(v, RawVerdict::Pass),
        "client_response 应放行: {v:?}"
    );
    assert!(has_xb(&m), "B 插件的 header 改写不得被吞");

    // upstream_response 成功路径（error=false）：重试无意义，同样按放行扇出
    let mut m = raw_msg(RawStage::UpstreamResponse, Some(200), false);
    let v = registry
        .on_upstream_response_raw(&ctx, &mut m)
        .await
        .unwrap();
    assert!(
        matches!(v, RawVerdict::Pass),
        "成功路径 retry 无意义，应放行: {v:?}"
    );
    assert!(has_xb(&m), "B 插件的 header 改写不得被吞");

    // upstream_response 错误路径（error=true）：retry 是终局判定——立即返回，
    // 同阶段靠后的 B 插件不执行（其改写也就不会出现）
    let mut m = raw_msg(RawStage::UpstreamResponse, Some(520), true);
    let v = registry
        .on_upstream_response_raw(&ctx, &mut m)
        .await
        .unwrap();
    assert_eq!(
        v,
        RawVerdict::Retry { delay_ms: 0 },
        "错误路径 retry 应立即生效: {v:?}"
    );
    assert!(!has_xb(&m), "错误路径上 B 插件不应被执行");
}
