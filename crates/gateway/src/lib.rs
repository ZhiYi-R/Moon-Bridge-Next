//! Moon Bridge Next 服务层（网关）。
//!
//! 组合 core/protocol/plugin/store，对外提供内嵌 HTTP 网关：
//! - [`dispatch`]：请求生命周期编排（RAW/CORE 双组钩子串联）。
//! - [`server`]：axum 路由与启动。
//! - [`router`]：模型别名 → provider/上游模型 的路由解析。
//! - [`stream`]：流式 SSE 编排。
//! - [`bridge`]：[`moonbridge_plugin::HostBridge`] 的网关实现。
//! - [`state`]：共享运行状态。
//!
//! 顶层入口：
//! - [`bootstrap`]：由引导配置 + 数据库构造 [`AppState`]（含插件加载）。
//! - [`serve`]：启动网关服务器。
//!
//! 依赖方向：gateway → core, protocol, plugin, store。

pub mod bridge;
pub mod config;
pub mod dispatch;
pub mod error;
pub mod handlers;
pub mod router;
pub mod server;
pub mod sse;
pub mod state;
pub mod stream;
pub mod trace;
pub mod upstream;
pub mod usage;

use std::sync::Arc;

use moonbridge_plugin::{LuaPluginRegistry, LuaRuntime, SandboxLimits, SessionStore};
use moonbridge_protocol::{builtin_registry, NoopHooks, PluginHooks, Registry};
use moonbridge_store::Database;

pub use config::GatewayConfig;
pub use error::{GatewayError, Result};
pub use state::AppState;

use crate::bridge::GatewayBridge;
use crate::upstream::build_client;

/// 从引导配置派生插件沙箱配额。
///
/// `max_body_bytes` 直接采用配置（兼现“报文层 body 上限”语义）；`call_timeout`
/// 以上游请求超时为基准再加缓冲，避免误伤插件内合法的 `provider_invoke` 长调用；
/// 指令上限/内存上限/计数步长沿用 [`SandboxLimits::default`]。
fn sandbox_limits(config: &GatewayConfig) -> SandboxLimits {
    SandboxLimits {
        max_body_bytes: config.max_body_bytes,
        call_timeout: std::time::Duration::from_secs(config.request_timeout_secs.saturating_add(30)),
        ..SandboxLimits::default()
    }
}

/// 从 store 加载 Lua 插件，构造 [`PluginHooks`]。
///
/// 加载条件：插件全局启用，或存在 enabled=1 的 provider 维度 binding
/// （全局停用但被某 Provider 强制启用的插件仍需加载）。运行时门控：
/// 请求命中 provider 后由 [`LuaPluginRegistry`] 按三态 binding 逐请求过滤。
/// 无插件或全部加载失败时回退 [`NoopHooks`]。单个插件加载失败仅记录错误，
/// 不影响其它插件与网关启动。
pub fn load_plugins(
    db: &Arc<Database>,
    registry: &Arc<Registry>,
    client: &reqwest::Client,
    limits: SandboxLimits,
) -> Arc<dyn PluginHooks> {
    let sessions = SessionStore::new();
    let bridge = Arc::new(GatewayBridge::new(
        client.clone(),
        db.clone(),
        registry.clone(),
    ));
    let mut runtimes: Vec<Arc<LuaRuntime>> = Vec::new();

    // provider 维度三态门控表：(plugin_name, provider_key) → enabled
    let mut overrides = std::collections::HashMap::new();
    match db.list_bindings_by_scope("provider") {
        Ok(bindings) => {
            for b in bindings {
                overrides.insert((b.plugin_name, b.scope_key), b.enabled);
            }
        }
        Err(e) => tracing::error!(error = %e, "读取 provider 维度插件绑定失败"),
    }

    match db.list_plugins() {
        Ok(plugins) => {
            for p in plugins {
                // 全局停用但被某 Provider 强制启用的插件仍需加载
                let force_enabled = overrides
                    .iter()
                    .any(|((name, _), en)| name == &p.name && *en);
                if !p.enabled && !force_enabled {
                    continue;
                }
                let script = read_script(&p.script_ref);
                match LuaRuntime::new_with_limits(
                    &p.name,
                    &script,
                    &p.config,
                    bridge.clone(),
                    sessions.clone(),
                    limits.clone(),
                ) {
                    Ok(mut rt) => {
                        rt.enabled = p.enabled;
                        let caps: Vec<String> =
                            rt.manifest.capabilities.iter().cloned().collect();
                        tracing::info!(plugin = %p.name, enabled = p.enabled, capabilities = ?caps, "已加载 Lua 插件");
                        runtimes.push(Arc::new(rt));
                    }
                    Err(e) => tracing::error!(plugin = %p.name, error = %e, "插件加载失败"),
                }
            }
        }
        Err(e) => tracing::error!(error = %e, "读取插件列表失败"),
    }

    if runtimes.is_empty() {
        Arc::new(NoopHooks)
    } else {
        Arc::new(LuaPluginRegistry::new(runtimes, overrides))
    }
}

/// 读取插件脚本：`script_ref` 为存在的文件路径则读文件，否则视为内联脚本内容。
fn read_script(script_ref: &str) -> String {
    let path = std::path::Path::new(script_ref);
    if path.exists() {
        std::fs::read_to_string(path).unwrap_or_else(|_| script_ref.to_string())
    } else {
        script_ref.to_string()
    }
}

/// 由引导配置 + 数据库构造网关共享状态（装配内置 Adapter、加载插件、构建 HTTP 客户端）。
pub fn bootstrap(config: GatewayConfig, db: Arc<Database>) -> Result<Arc<AppState>> {
    let registry = Arc::new(builtin_registry());
    let client = build_client(&config)?;
    let limits = sandbox_limits(&config);
    let hooks = load_plugins(&db, &registry, &client, limits);
    Ok(AppState::new(config, db, registry, hooks, client))
}

/// 启动网关服务器（阻塞直到退出）。
pub async fn serve(config: GatewayConfig, db: Arc<Database>) -> Result<()> {
    let state = bootstrap(config, db)?;
    server::serve(state).await
}
