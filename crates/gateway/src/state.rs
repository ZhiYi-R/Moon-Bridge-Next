//! 网关运行状态：跨请求共享的依赖聚合。

use std::sync::Arc;

use moonbridge_protocol::{PluginHooks, Registry};
use moonbridge_store::Database;

use crate::config::GatewayConfig;
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
    /// 共享上游 HTTP 客户端（已施加 egress 代理）。
    pub client: reqwest::Client,
    /// 活跃会话表（深度取 `config.session_table_depth`，见 `crate::session`）。
    pub sessions: SessionTable,
}

impl AppState {
    /// 构造共享状态。会话表按配置深度就地建好，故不额外收参数。
    pub fn new(
        config: GatewayConfig,
        db: Arc<Database>,
        registry: Arc<Registry>,
        hooks: Arc<dyn PluginHooks>,
        client: reqwest::Client,
    ) -> Arc<Self> {
        let sessions = SessionTable::new(config.session_table_depth);
        Arc::new(AppState {
            config,
            db,
            registry,
            hooks,
            client,
            sessions,
        })
    }
}
