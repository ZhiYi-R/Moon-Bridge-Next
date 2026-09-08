//! Tauri 托管的应用状态：数据库、引导配置、网关生命周期句柄。
//!
//! commands 与 tray 通过 `State<Arc<ManagedState>>` 访问；网关以 tokio task +
//! oneshot 优雅关闭信号驱动，实现「启动 / 停止 / 查询状态」。

use std::sync::{Arc, Mutex, RwLock};

use anyhow::{Context, Result};
use moonbridge_store::Database;
use serde::Serialize;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::config::{AppConfig, AppPaths};

/// 运行中的网关句柄：优雅关闭信号发送端 + 服务 task + 监听地址。
struct GatewayHandle {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
    addr: String,
}

/// 网关运行状态（回传前端）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayStatus {
    pub running: bool,
    pub addr: String,
    /// 最近一次启动失败的错误信息（若有）。
    pub error: Option<String>,
}

/// 应用共享状态。
pub struct ManagedState {
    /// SQLite 存储句柄。
    pub db: Arc<Database>,
    /// 关键路径集合。
    pub paths: AppPaths,
    /// 引导配置。
    config: RwLock<AppConfig>,
    /// 运行中的网关（`None` 表示未启动）。
    gateway: Mutex<Option<GatewayHandle>>,
    /// 最近一次网关错误。
    last_error: Mutex<Option<String>>,
}

impl ManagedState {
    /// 构造状态：打开数据库、加载引导配置。
    pub fn new(paths: AppPaths) -> Result<Arc<Self>> {
        paths.ensure_dirs().context("初始化应用目录失败")?;
        let db = Database::open(&paths.db_path)
            .with_context(|| format!("打开数据库失败: {}", paths.db_path.display()))?;
        let config = AppConfig::load_or_default(&paths.config_file).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "引导配置加载失败，使用默认配置");
            AppConfig::default()
        });
        // 把 trace 目录回填进网关配置（若用户未显式设置）
        let mut config = config;
        if config.gateway.trace_dir.is_none() {
            config.gateway.trace_dir = Some(paths.trace_dir.to_string_lossy().to_string());
        }
        Ok(Arc::new(Self {
            db: Arc::new(db),
            paths,
            config: RwLock::new(config),
            gateway: Mutex::new(None),
            last_error: Mutex::new(None),
        }))
    }

    /// 当前引导配置的快照。
    pub fn config(&self) -> AppConfig {
        self.config.read().unwrap().clone()
    }

    /// 更新引导配置并落盘。
    pub fn update_config(&self, f: impl FnOnce(&mut AppConfig)) -> Result<()> {
        let mut guard = self.config.write().unwrap();
        f(&mut guard);
        let snapshot = guard.clone();
        drop(guard);
        snapshot.save(&self.paths.config_file)
    }

    /// 查询网关状态（同步，不阻塞）。
    pub fn status(&self) -> GatewayStatus {
        let addr = self.config.read().unwrap().gateway.addr.clone();
        let error = self.last_error.lock().unwrap().clone();
        let running = self
            .gateway
            .lock()
            .unwrap()
            .as_ref()
            .map(|h| !h.task.is_finished())
            .unwrap_or(false);
        GatewayStatus {
            running,
            addr: if running {
                self.gateway
                    .lock()
                    .unwrap()
                    .as_ref()
                    .map(|h| h.addr.clone())
                    .unwrap_or(addr)
            } else {
                addr
            },
            error: if running { None } else { error },
        }
    }

    /// 启动网关（若已运行则直接返回当前状态）。
    pub async fn start_gateway(&self) -> Result<GatewayStatus> {
        // 已在运行则幂等返回
        if let Some(h) = self.gateway.lock().unwrap().as_ref() {
            if !h.task.is_finished() {
                return Ok(self.status());
            }
        }

        let config = self.config.read().unwrap().gateway.clone();
        let db = self.db.clone();
        let state = moonbridge_gateway::bootstrap(config, db).context("构建网关状态失败")?;
        let addr = state.config.addr.clone();

        let (tx, rx) = oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            let shutdown = async move {
                let _ = rx.await;
            };
            if let Err(e) = moonbridge_gateway::server::serve_with_shutdown(state, shutdown).await {
                tracing::error!(error = %e, "网关服务异常退出");
            }
        });

        // 稍等片刻确认端口成功绑定（task 未立即结束）
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        if task.is_finished() {
            *self.last_error.lock().unwrap() = Some(format!("网关启动失败：地址 {addr} 可能被占用"));
            *self.gateway.lock().unwrap() = None;
            return Ok(self.status());
        }

        *self.last_error.lock().unwrap() = None;
        *self.gateway.lock().unwrap() = Some(GatewayHandle {
            shutdown: tx,
            task,
            addr,
        });
        Ok(self.status())
    }

    /// 停止网关（优雅关闭并等待 task 退出）。
    pub async fn stop_gateway(&self) -> Result<GatewayStatus> {
        let handle = self.gateway.lock().unwrap().take();
        if let Some(h) = handle {
            let _ = h.shutdown.send(());
            let _ = h.task.await;
        }
        Ok(self.status())
    }
}
