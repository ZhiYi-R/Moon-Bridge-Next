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
use axum::Router;
use tower_http::services::{ServeDir, ServeFile};

use crate::admin::{self, AdminState};
use crate::args::ServerOpts;
use crate::config::{AppConfig, AppPaths};

/// 目录拉取客户端超时（与桌面端 catalog 口径一致）。
const CATALOG_TIMEOUT_SECS: u64 = 30;

/// 引导并运行服务端（阻塞到 `Ctrl-C` 或进程被 signal 终止）。
pub async fn run(opts: ServerOpts) -> Result<()> {
    let (paths, app_config) = prepare(&opts)?;

    let db = moonbridge_store::Database::open(&paths.db_path)
        .with_context(|| format!("打开数据库失败: {}", paths.db_path.display()))?;
    let db = Arc::new(db);

    let admin_state = AdminState {
        db: db.clone(),
        paths: paths.clone(),
        config: Arc::new(RwLock::new(app_config.clone())),
        admin_token: Arc::new(opts.admin_token.clone()),
        catalog: reqwest::Client::builder()
            .timeout(Duration::from_secs(CATALOG_TIMEOUT_SECS))
            .build()
            .context("构建 models.dev 拉取客户端失败")?,
    };

    let gateway_state = moonbridge_gateway::bootstrap(app_config.gateway.clone(), db.clone())
        .context("构建网关状态失败")?;
    let addr = gateway_state.config.addr.clone();

    // 余额看板调度：独立于网关启停（网关不启动也持续跑），随进程存活。
    let balance_engine =
        moonbridge_gateway::BalanceEngine::new(db.clone(), Some(paths.plugins_dir.clone()));
    let _balance_scheduler = moonbridge_gateway::spawn_balance_scheduler(
        db.clone(),
        balance_engine,
        Some(paths.plugins_dir.clone()),
    );

    let mut merged: Router<()> =
        moonbridge_gateway::server::router(gateway_state.clone()).merge(admin::router(admin_state));
    // 静态托管：目录不存在时只告警并跳过，API 与网关照常可用。
    match static_service(&opts.web_dir) {
        Some(service) => {
            merged = merged.fallback_service(service);
        }
        None => {
            tracing::warn!(
                dir = %opts.web_dir.display(),
                "前端静态目录不存在，已跳过静态托管（网关与管理 API 仍可用）"
            );
        }
    }

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("监听 {addr} 失败"))?;
    tracing::info!(%addr, "Moon Bridge Next 服务端已启动（网关 + 管理 API + 静态托管）");

    // [LIFECYCLE] 绑定成功后、开始服务前跑 MB.init；绑定失败不该留下
    // 「init 跑过但 shutdown 永不跑」的不配对状态。
    gateway_state.hooks.init_all().await;
    let served = axum::serve(listener, merged)
        .with_graceful_shutdown(shutdown_signal())
        .await;
    // [LIFECYCLE] 无论服务如何退出都给插件一次收尾机会（与 init_all 严格配对）。
    gateway_state.hooks.shutdown_all().await;
    served.context("HTTP 服务错误")?;
    tracing::info!(%addr, "Moon Bridge Next 服务端已停止");
    Ok(())
}

/// 派生路径、加载配置、施加命令行覆盖。返回值同时用于数据库与网关引导。
fn prepare(opts: &ServerOpts) -> Result<(AppPaths, AppConfig)> {
    if opts.admin_token.trim().is_empty() {
        anyhow::bail!("admin token 不能为空");
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
    let mut config = AppConfig::load_or_default(&paths.config_file).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "引导配置加载失败，使用默认配置");
        AppConfig::default()
    });
    if let Some(addr) = &opts.addr {
        config.gateway.addr = addr.clone();
    }
    // 显式传入优先：覆盖配置文件，且不回写（日志里也绝不打印 token 本体）。
    config.gateway.auth_token = Some(opts.admin_token.clone());
    if config.gateway.trace_dir.is_none() {
        config.gateway.trace_dir = Some(paths.trace_dir.to_string_lossy().to_string());
    }
    if config.gateway.plugins_dir.is_none() {
        config.gateway.plugins_dir = Some(paths.plugins_dir.to_string_lossy().to_string());
    }
    Ok((paths, config))
}

/// 构造静态文件服务（SPA）：命中的文件直接返回，未命中的路径回落到 `index.html`
/// （前端路由接管），目录请求补 `index.html`。
///
/// `web_dir` 不存在或不是目录时返回 `None`（调用方只告警并跳过静态托管）。
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

/// 关闭信号：`Ctrl-C` 或 `SIGTERM`（容器停止）。
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
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

    #[test]
    fn prepare_rejects_blank_admin_token() {
        let dir = temp_dir("prepare-blank");
        let opts = ServerOpts {
            admin_token: "   ".to_string(),
            addr: None,
            config_dir: Some(dir.join("cfg")),
            data_dir: Some(dir.join("data")),
            web_dir: dir.join("web"),
        };
        let err = prepare(&opts).unwrap_err();
        assert!(err.to_string().contains("admin token 不能为空"), "{err:#}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prepare_applies_cli_overrides_and_derives_dirs() {
        let dir = temp_dir("prepare-ok");
        let config_dir = dir.join("cfg");
        let data_dir = dir.join("data");
        let opts = ServerOpts {
            admin_token: "boot-token".to_string(),
            addr: Some("127.0.0.1:12345".to_string()),
            config_dir: Some(config_dir.clone()),
            data_dir: Some(data_dir.clone()),
            web_dir: dir.join("web"),
        };
        let (paths, cfg) = prepare(&opts).unwrap();
        assert_eq!(paths.config_file, config_dir.join("config.toml"));
        assert_eq!(paths.db_path, data_dir.join("moonbridge.db"));
        // 命令行覆盖落到运行配置，且 token 只驻留内存（此处不落盘，由 admin 侧断言）
        assert_eq!(cfg.gateway.addr, "127.0.0.1:12345");
        assert_eq!(cfg.gateway.auth_token.as_deref(), Some("boot-token"));
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
}
