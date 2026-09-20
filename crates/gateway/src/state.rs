//! 网关运行状态：跨请求共享的依赖聚合。

use std::collections::HashMap;
use std::sync::Arc;

use moonbridge_plugin::LuaPluginRegistry;
use moonbridge_protocol::{PluginHooks, Registry};
use moonbridge_store::Database;

use crate::config::GatewayConfig;
use crate::oauth::CallbackRegistry;
use crate::session::SessionTable;

/// 网关共享状态。以 `Arc<AppState>` 注入 axum handler。
pub struct AppState {
    /// 引导配置（监听地址、认证、egress 代理、配额）。
    pub config: GatewayConfig,
    /// 配置/用量存储。
    pub db: Arc<Database>,
    /// 协议 Adapter 注册表。
    pub registry: Arc<Registry>,
    /// 插件钩子（Lua 注册表或 Noop）。
    pub hooks: Arc<dyn PluginHooks>,
    /// Lua 插件注册表（无插件时为 None）：dispatch 按 provider 解析 CAP_AUTH 插件用。
    pub plugin_registry: Option<Arc<LuaPluginRegistry>>,
    /// 共享上游 HTTP 客户端（已施加 egress 代理）。
    pub client: reqwest::Client,
    /// 活跃会话表（深度取 `config.session_table_depth`，见 `crate::session`）。
    pub sessions: SessionTable,
    /// OAuth 回环回调监听注册表（mb.oauth.listen_callback 的宿主实现）。
    pub callbacks: Arc<CallbackRegistry>,
    /// 认证令牌刷新单飞锁（按 provider 键；并发请求不双刷同一 refresh token）。
    pub auth_locks: std::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// 认证刷新失败退避表（provider 键 → 退避截止 unix 毫秒）。
    pub auth_backoff: std::sync::Mutex<HashMap<String, i64>>,
}

impl AppState {
    /// 构造共享状态。会话表按配置深度就地建好，故不额外收参数。
    pub fn new(
        config: GatewayConfig,
        db: Arc<Database>,
        registry: Arc<Registry>,
        hooks: Arc<dyn PluginHooks>,
        plugin_registry: Option<Arc<LuaPluginRegistry>>,
        client: reqwest::Client,
        callbacks: Arc<CallbackRegistry>,
    ) -> Arc<Self> {
        let sessions = SessionTable::new(config.session_table_depth);
        Arc::new(AppState {
            config,
            db,
            registry,
            hooks,
            plugin_registry,
            client,
            sessions,
            callbacks,
            auth_locks: std::sync::Mutex::new(HashMap::new()),
            auth_backoff: std::sync::Mutex::new(HashMap::new()),
        })
    }
}
