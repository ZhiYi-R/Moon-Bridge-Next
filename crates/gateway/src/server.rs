//! axum 路由装配与服务器启动。

use std::future::Future;
use std::sync::Arc;

use axum::routing::{get, post};
use axum::Router;
use tower_http::cors::CorsLayer;

use crate::error::{GatewayError, Result};
use crate::handlers;
use crate::state::AppState;

/// 构建 axum 路由。
///
/// 入口端点覆盖三种协议（多协议入口）：
/// - `/v1/responses`（+ `/responses`）—— OpenAI Responses
/// - `/v1/messages` —— Anthropic Messages
/// - `/v1/chat/completions` —— OpenAI Chat
///
/// 另有 `/health`、`/v1/models`。Gemini 作入口时未挂路由（主要作上游）。
pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health", get(handlers::health))
        .route("/v1/responses", post(handlers::responses))
        .route("/responses", post(handlers::responses))
        .route("/v1/messages", post(handlers::messages))
        .route("/v1/chat/completions", post(handlers::chat_completions))
        .route("/v1/models", get(handlers::models))
        .route("/models", get(handlers::models))
        .layer(CorsLayer::permissive())
        .with_state(state)
}

/// 绑定并启动 HTTP 服务器（阻塞直到服务退出）。
pub async fn serve(state: Arc<AppState>) -> Result<()> {
    serve_with_shutdown(state, std::future::pending::<()>()).await
}

/// 绑定并启动 HTTP 服务器，`shutdown` future 完成时优雅关闭。
///
/// app 层（src-tauri）用 oneshot channel 驱动 `shutdown`，实现网关的启停控制。
pub async fn serve_with_shutdown<F>(state: Arc<AppState>, shutdown: F) -> Result<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    let addr = state.config.addr.clone();
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|e| GatewayError::Other(format!("监听 {addr} 失败: {e}")))?;
    tracing::info!(%addr, "Moon Bridge Next 网关已启动");
    // [LIFECYCLE] 绑定成功后、开始服务前跑 MB.init。放在这里而非 bootstrap：
    // bootstrap 是同步函数，await 不了 Lua 钩子；且绑定失败时不该留下
    // 「init 跑过但 shutdown 永不跑」的不配对状态。
    state.hooks.init_all().await;
    let served = axum::serve(listener, router(state.clone()))
        .with_graceful_shutdown(shutdown)
        .await;
    // [LIFECYCLE] 无论服务如何退出都给插件一次收尾机会（与 init_all 严格配对）。
    state.hooks.shutdown_all().await;
    served.map_err(|e| GatewayError::Other(format!("网关服务错误: {e}")))?;
    tracing::info!(%addr, "Moon Bridge Next 网关已停止");
    Ok(())
}
