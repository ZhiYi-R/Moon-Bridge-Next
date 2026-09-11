//! LuaPluginRegistry provider 维度三态门控单测。
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
use moonbridge_protocol::{PluginHooks, ReqCtx};

/// 不做任何事的 HostBridge 桩。
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

/// 造只含一组 provider 绑定的覆盖表。
fn provider_overrides(bindings: &[(&str, bool)]) -> std::collections::HashMap<String, ScopeOverrides> {
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
    let overrides =
        provider_overrides(&[("off-provider", false), ("on-provider", true)]);
    let registry = LuaPluginRegistry::new(vec![probe_runtime(true)], overrides, SessionStore::new());

    // binding 禁用 → 覆盖全局启用
    let mut req = CoreRequest::new("m");
    req.temperature = Some(1.0);
    registry.on_request(&ctx_for(Some("off-provider")), &mut req).await.unwrap();
    assert_eq!(req.temperature, Some(1.0), "provider 级禁用应生效");

    // binding 启用 → 与全局一致，钩子执行
    let mut req = CoreRequest::new("m");
    registry.on_request(&ctx_for(Some("on-provider")), &mut req).await.unwrap();
    assert_eq!(req.temperature, Some(0.5));
}

#[tokio::test]
async fn no_binding_follows_global() {
    let registry =
        LuaPluginRegistry::new(vec![probe_runtime(true)], Default::default(), SessionStore::new());

    // 无 binding：跟随全局启用
    let mut req = CoreRequest::new("m");
    registry.on_request(&ctx_for(Some("any-provider")), &mut req).await.unwrap();
    assert_eq!(req.temperature, Some(0.5));
}

#[tokio::test]
async fn force_enable_disabled_plugin() {
    let overrides = provider_overrides(&[("p", true)]);
    let registry = LuaPluginRegistry::new(vec![probe_runtime(false)], overrides, SessionStore::new());

    // 全局停用 + provider 强制启用 → 执行
    let mut req = CoreRequest::new("m");
    registry.on_request(&ctx_for(Some("p")), &mut req).await.unwrap();
    assert_eq!(req.temperature, Some(0.5));

    // 全局停用 + 其它 provider（无 binding）→ 不执行
    let mut req = CoreRequest::new("m");
    registry.on_request(&ctx_for(Some("other")), &mut req).await.unwrap();
    assert_eq!(req.temperature, None);
}

#[tokio::test]
async fn client_stage_uses_global_only() {
    let overrides = provider_overrides(&[("off-provider", false)]);
    let registry = LuaPluginRegistry::new(vec![probe_runtime(true)], overrides, SessionStore::new());

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
    let registry = LuaPluginRegistry::new(vec![probe_runtime(true)], overrides, SessionStore::new());

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
    assert_eq!(req.temperature, Some(0.5), "global 维度应兜底");
}
