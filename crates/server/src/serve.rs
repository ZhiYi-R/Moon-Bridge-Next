//! 合并路由与进程生命周期。
//!
//! 三个 Router 各自 `with_state` 后 merge 为一个 `Router<()>`：LLM 网关路由、
//! `/api/*` 管理 API、前端静态文件（SPA fallback）。语义复刻
//! [`moonbridge_gateway::server::serve_with_shutdown`]：绑定端口成功 → 插件
//! `init` → serve → 优雅关闭 → 插件 `shutdown`。

use std::path::Path;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::{Context, Result};
use axum::extract::Request;
use axum::http::{header, HeaderValue};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::Router;
use tower_http::services::{ServeDir, ServeFile};

use crate::admin::{self, AdminState};
use crate::args::ServerOpts;
use crate::config::{AppConfig, AppPaths};
use crate::lifecycle::Lifecycle;

const CATALOG_TIMEOUT_SECS: u64 = 30;

/// 收到 drain 信号（重启 / 停止）后，等待在途连接自然收尾的上限；超时则强制关闭。
/// axum 的 graceful shutdown 本身无上限，一个卡住的长连 SSE 流会让重启 / SIGTERM 永久挂起。
const DRAIN_GRACE: Duration = Duration::from_secs(30);

pub async fn run(opts: ServerOpts) -> Result<()> {
    let (paths, app_config, persisted_config) = prepare(&opts)?;
    let db = match &opts.key_file {
        Some(key_file) => moonbridge_store::Database::open_with_key_file(&paths.db_path, key_file),
        None => moonbridge_store::Database::open(&paths.db_path),
    }
    .with_context(|| format!("打开数据库失败: {}", paths.db_path.display()))?;
    let db = Arc::new(db);
    let lifecycle = Arc::new(Lifecycle::default());
    let admin_state = AdminState {
        db: db.clone(),
        paths: paths.clone(),
        config: Arc::new(RwLock::new(app_config)),
        persisted_config: Arc::new(RwLock::new(persisted_config)),
        lifecycle: lifecycle.clone(),
        admin_token: Arc::new(opts.admin_token.clone()),
        catalog: reqwest::Client::builder()
            .timeout(Duration::from_secs(CATALOG_TIMEOUT_SECS))
            .build()
            .context("构建 models.dev 拉取客户端失败")?,
    };
    let signal_task = listen_for_shutdown(lifecycle.clone())?;
    let result = serve_generations(&opts.web_dir, admin_state).await;
    lifecycle.finish(result.is_err());
    signal_task.abort();
    let _ = signal_task.await;
    result
}

async fn serve_generations(web_dir: &Path, admin_state: AdminState) -> Result<()> {
    let lifecycle = admin_state.lifecycle.clone();
    while lifecycle.begin_start() {
        let gateway_state =
            moonbridge_gateway::bootstrap(admin_state.config().gateway, admin_state.db.clone())
                .map_err(|_| anyhow::anyhow!("构建网关状态失败"))?;
        if lifecycle.stopping() {
            break;
        }
        let listener = tokio::net::TcpListener::bind(&gateway_state.config.addr)
            .await
            .context("绑定服务监听地址失败")?;
        let local_addr = listener.local_addr().context("读取服务监听地址失败")?;
        if lifecycle.stopping() {
            break;
        }
        let mut merged: Router<()> = moonbridge_gateway::server::router(gateway_state.clone())
            .merge(admin::router(admin_state.clone()));
        match static_service(web_dir) {
            Some(service) => merged = merged.fallback_service(service),
            None => tracing::warn!("前端静态目录不存在，已跳过静态托管"),
        }
        merged = merged.layer(middleware::from_fn(security_headers));
        gateway_state.hooks.init_all().await;
        let balance_policy = moonbridge_gateway::BalanceNetworkPolicy::from_environment(
            gateway_state.config.egress_proxy.clone(),
        );
        if let Some(reason) = balance_policy.denied_reason() {
            tracing::warn!(
                reason,
                "余额看板已整体禁用：每次查询都会失败。请检查 gateway.egress_proxy 与 MOONBRIDGE_BALANCE_PRIVATE_ORIGINS 配置"
            );
        }
        let balance_engine = moonbridge_gateway::BalanceEngine::new(
            admin_state.db.clone(),
            Some(admin_state.paths.plugins_dir.clone()),
        )
        .with_network_policy(balance_policy);
        let balance_scheduler = moonbridge_gateway::spawn_balance_scheduler(
            admin_state.db.clone(),
            balance_engine,
            Some(admin_state.paths.plugins_dir.clone()),
        );
        lifecycle.listening(local_addr);
        tracing::info!(addr = %local_addr, "Moon Bridge Next 服务端已启动");
        let shutdown = lifecycle.clone();
        let server = axum::serve(listener, merged)
            .with_graceful_shutdown(async move { shutdown.wait_for_drain().await });
        // 兜底：drain 信号到达后给在途连接 DRAIN_GRACE 收尾，超时则丢弃 server future
        // （关闭监听与剩余连接）。否则一个卡住的长连 SSE 流会让重启 / SIGTERM 永久挂起。
        let force = lifecycle.clone();
        let served = tokio::select! {
            result = server => result,
            _ = async {
                force.wait_for_drain().await;
                tokio::time::sleep(DRAIN_GRACE).await;
            } => {
                tracing::warn!(
                    grace_secs = DRAIN_GRACE.as_secs(),
                    "优雅 drain 超时，强制关闭剩余在途连接"
                );
                Ok(())
            }
        };
        lifecycle.drained();
        balance_scheduler.abort();
        let _ = balance_scheduler.await;
        gateway_state.hooks.shutdown_all().await;
        served.context("HTTP 服务错误")?;
        tracing::info!(addr = %local_addr, "Moon Bridge Next 服务端已停止");
        if lifecycle.stopping() {
            break;
        }
    }
    Ok(())
}

fn prepare(opts: &ServerOpts) -> Result<(AppPaths, AppConfig, AppConfig)> {
    if opts.admin_token.trim().is_empty() {
        anyhow::bail!("admin token 不能为空");
    }
    if !moonbridge_gateway::auth::token_is_valid(&opts.admin_token) {
        anyhow::bail!("admin token 必须是非空且不含空白的可打印 ASCII 字符");
    }
    let config_dir = opts
        .config_dir
        .clone()
        .or_else(|| dirs::config_dir().map(|d| d.join(crate::args::APP_DIR_NAME)));
    let data_dir = opts
        .data_dir
        .clone()
        .or_else(|| dirs::data_dir().map(|d| d.join(crate::args::APP_DIR_NAME)));
    let (Some(config_dir), Some(data_dir)) = (config_dir, data_dir) else {
        anyhow::bail!("无法确定平台默认目录，请显式传入 --config-dir 与 --data-dir");
    };
    let paths = AppPaths::resolve(config_dir, data_dir);
    paths.ensure_dirs().context("初始化应用目录失败")?;
    let mut persisted = AppConfig::load_or_default(&paths.config_file)?;
    let mut config = persisted.clone();
    config.gateway.auth_token = opts
        .gateway_token
        .clone()
        .or(config.gateway.auth_token)
        .map(|token| token.trim().to_string());
    match config.gateway.auth_token.as_deref() {
        Some(token)
            if moonbridge_gateway::auth::token_is_valid(token)
                && token != opts.admin_token.trim() => {}
        _ => anyhow::bail!("gateway token 必须非空且不同于 admin token"),
    }
    if persisted.gateway.auth_token.as_deref().map(str::trim) == Some(opts.admin_token.trim()) {
        persisted.gateway.auth_token = None;
    }
    if let Some(addr) = &opts.addr {
        config.gateway.addr = addr.clone();
    }
    if config.gateway.trace_dir.is_none() {
        config.gateway.trace_dir = Some(paths.trace_dir.to_string_lossy().to_string());
    }
    if config.gateway.plugins_dir.is_none() {
        config.gateway.plugins_dir = Some(paths.plugins_dir.to_string_lossy().to_string());
    }
    Ok((paths, config, persisted))
}

fn static_service(web_dir: &Path) -> Option<ServeDir<ServeFile>> {
    if !web_dir.is_dir() {
        return None;
    }
    Some(
        ServeDir::new(web_dir)
            .append_index_html_on_directories(true)
            .fallback(ServeFile::new(web_dir.join("index.html"))),
    )
}

async fn security_headers(req: Request, next: Next) -> Response {
    let mut response = next.run(req).await;
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self' data:; connect-src 'self'; object-src 'none'; base-uri 'self'; frame-ancestors 'none'; form-action 'self'",
        ),
    );
    response
}

fn listen_for_shutdown(lifecycle: Arc<Lifecycle>) -> Result<tokio::task::JoinHandle<()>> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut interrupt = signal(SignalKind::interrupt()).context("注册 SIGINT 监听失败")?;
        let mut terminate = signal(SignalKind::terminate()).context("注册 SIGTERM 监听失败")?;
        Ok(tokio::spawn(async move {
            tokio::select! {
                _ = interrupt.recv() => {},
                _ = terminate.recv() => {},
            }
            lifecycle.request_stop();
        }))
    }
    #[cfg(not(unix))]
    {
        Ok(tokio::spawn(async move {
            let _ = tokio::signal::ctrl_c().await;
            lifecycle.request_stop();
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "moonbridge-server-serve-{}-{tag}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn static_service_is_some_for_existing_directory() {
        let dir = temp_dir("web-ok");
        std::fs::write(dir.join("index.html"), "<html></html>").unwrap();
        assert!(static_service(&dir).is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn static_service_is_none_for_missing_or_non_directory() {
        let dir = temp_dir("web-missing");
        assert!(
            static_service(&dir.join("nope")).is_none(),
            "目录不存在应跳过静态托管"
        );

        let file = dir.join("index.html");
        std::fs::write(&file, "<html></html>").unwrap();
        assert!(
            static_service(&file).is_none(),
            "路径是文件而非目录同样跳过"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn opts_for(dir: &Path) -> ServerOpts {
        ServerOpts {
            admin_token: "admin-private-token".to_string(),
            gateway_token: Some("gateway-private-token".to_string()),
            key_file: None,
            addr: None,
            config_dir: Some(dir.join("cfg")),
            data_dir: Some(dir.join("data")),
            web_dir: dir.join("web"),
        }
    }

    #[test]
    fn prepare_rejects_blank_admin_token() {
        let dir = temp_dir("prepare-blank");
        let mut opts = opts_for(&dir);
        opts.admin_token = "   ".to_string();
        let err = prepare(&opts).unwrap_err();
        assert!(err.to_string().contains("admin token 不能为空"), "{err:#}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prepare_applies_cli_overrides_and_derives_dirs() {
        let dir = temp_dir("prepare-ok");
        let mut opts = opts_for(&dir);
        opts.addr = Some("127.0.0.1:12345".to_string());
        let (paths, cfg, persisted) = prepare(&opts).unwrap();
        assert_eq!(paths.config_file, dir.join("cfg/config.toml"));
        assert_eq!(paths.db_path, dir.join("data/moonbridge.db"));
        assert_eq!(cfg.gateway.addr, "127.0.0.1:12345");
        assert_eq!(
            cfg.gateway.auth_token.as_deref(),
            Some("gateway-private-token")
        );
        assert!(persisted.gateway.auth_token.is_none());
        assert_eq!(persisted.gateway.addr, AppConfig::default().gateway.addr);
        assert!(persisted.gateway.trace_dir.is_none());
        assert!(persisted.gateway.plugins_dir.is_none());
        assert!(!paths.config_file.exists(), "启动覆盖不得落盘");
        assert_eq!(
            cfg.gateway.trace_dir.as_deref(),
            Some(paths.trace_dir.to_string_lossy().as_ref())
        );
        assert_eq!(
            cfg.gateway.plugins_dir.as_deref(),
            Some(paths.plugins_dir.to_string_lossy().as_ref())
        );
        assert!(paths.trace_dir.is_dir() && paths.plugins_dir.is_dir());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prepare_keeps_persisted_gateway_token_separate_from_override() {
        let dir = temp_dir("prepare-persisted");
        let mut opts = opts_for(&dir);
        let paths = AppPaths::resolve(dir.join("cfg"), dir.join("data"));
        paths.ensure_dirs().unwrap();
        let mut saved = AppConfig::default();
        saved.gateway.auth_token = Some("saved-gateway-token".into());
        saved.save(&paths.config_file).unwrap();
        let before = std::fs::read(&paths.config_file).unwrap();
        let (_, runtime, persisted) = prepare(&opts).unwrap();
        assert_eq!(runtime.gateway.auth_token, opts.gateway_token);
        assert_eq!(persisted.gateway.auth_token, saved.gateway.auth_token);
        assert_eq!(std::fs::read(&paths.config_file).unwrap(), before);
        opts.gateway_token = None;
        let (_, runtime, persisted) = prepare(&opts).unwrap();
        assert_eq!(runtime.gateway.auth_token, saved.gateway.auth_token);
        assert_eq!(persisted.gateway.auth_token, saved.gateway.auth_token);
        assert_eq!(std::fs::read(&paths.config_file).unwrap(), before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prepare_rejects_missing_blank_or_admin_equivalent_gateway_token() {
        let dir = temp_dir("prepare-invalid-gateway");
        let mut opts = opts_for(&dir);
        for token in [
            None,
            Some(""),
            Some("  "),
            Some("admin-private-token"),
            Some(" admin-private-token "),
        ] {
            opts.gateway_token = token.map(str::to_string);
            let err = prepare(&opts).unwrap_err();
            assert!(err.to_string().contains("gateway token"));
            assert!(!format!("{err:#}").contains("admin-private-token"));
        }
        let paths = AppPaths::resolve(dir.join("cfg"), dir.join("data"));
        let mut saved = AppConfig::default();
        saved.gateway.auth_token = Some(opts.admin_token.clone());
        saved.save(&paths.config_file).unwrap();
        opts.gateway_token = None;
        assert!(prepare(&opts).is_err());
        opts.gateway_token = Some("valid-gateway-token".into());
        let (_, runtime, persisted) = prepare(&opts).unwrap();
        assert_eq!(
            runtime.gateway.auth_token.as_deref(),
            Some("valid-gateway-token")
        );
        assert!(
            persisted.gateway.auth_token.is_none(),
            "不得重新持久化旧管理凭据"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
