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
    /// 启停串行化：`has_live_gateway` 检查与句柄存储之间有 await，两次并发
    /// start 会都走到 spawn —— 败者 task 绑定失败时把 `gateway` 置 None，
    /// 恰好抹掉胜者刚存的句柄 ⇒ 网关在跑却停不掉、status 显示未运行、
    /// 再启动必报「地址被占用」。start/stop 全程持此锁即消除该 TOCTOU。
    lifecycle: tokio::sync::Mutex<()>,
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
        // 把 trace / plugins 目录回填进网关配置（若用户未显式设置）
        let mut config = config;
        if config.gateway.trace_dir.is_none() {
            config.gateway.trace_dir = Some(paths.trace_dir.to_string_lossy().to_string());
        }
        if config.gateway.plugins_dir.is_none() {
            config.gateway.plugins_dir = Some(paths.plugins_dir.to_string_lossy().to_string());
        }
        Ok(Arc::new(Self {
            db: Arc::new(db),
            paths,
            config: RwLock::new(config),
            gateway: Mutex::new(None),
            last_error: Mutex::new(None),
            lifecycle: tokio::sync::Mutex::new(()),
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
        let config_addr = self.config.read().unwrap().gateway.addr.clone();
        let error = self.last_error.lock().unwrap().clone();
        // running 与 addr 在同一把锁内一次取齐：原先分两次 lock 既冗余，
        // 又给「持锁再进 status」留了坑（见 `has_live_gateway`）。
        let (running, handle_addr) = {
            let guard = self.gateway.lock().unwrap();
            match guard.as_ref() {
                Some(h) if !h.task.is_finished() => (true, Some(h.addr.clone())),
                _ => (false, None),
            }
        };
        GatewayStatus {
            running,
            addr: handle_addr.unwrap_or(config_addr),
            error: if running { None } else { error },
        }
    }

    /// 是否已有存活的网关任务。
    ///
    /// 单独成函数是为了让 `MutexGuard` 在**返回时**必然释放。历史缺陷：
    /// `if let Some(h) = self.gateway.lock().unwrap().as_ref() { return self.status(); }`
    /// 在 edition 2021 下 `if let`  scrutinee 的临时量活到块结束，守卫仍被持有，
    /// 而 `status()` 再次获取同一把**非重入** `std::sync::Mutex` ⇒ 永久死锁，
    /// 连带 `gateway_status` / `gateway_stop` / 托盘切换全部卡死。
    /// 实测 edition 2021 与 2024 都会死锁，升级 edition 不是解法。
    fn has_live_gateway(&self) -> bool {
        let guard = self.gateway.lock().unwrap();
        matches!(guard.as_ref(), Some(h) if !h.task.is_finished())
    }

    /// 启动网关（若已运行则直接返回当前状态）。
    ///
    /// 与 `stop_gateway` 共用 `lifecycle` 锁串行化——check→spawn→store 不可被
    /// 并发启停插队（否则并发双 start 会让后启动的 task 绑定失败并清掉先成功
    /// 者刚存的句柄，网关在跑却失去句柄）。
    pub async fn start_gateway(&self) -> Result<GatewayStatus> {
        let _lifecycle = self.lifecycle.lock().await;
        // 已在运行则幂等返回（守卫已在 `has_live_gateway` 返回时释放）
        if self.has_live_gateway() {
            return Ok(self.status());
        }

        let mut config = self.config.read().unwrap().gateway.clone();
        // 路径型配置一律以 AppPaths 为准回填。设置页保存是**整体替换** AppConfig，
        // 前端漏传某字段就会把它变成 None —— 而 plugins_dir 为 None 时脚本包含性
        // 校验随之失效，绝不能依赖前端回传。
        if config.trace_dir.is_none() {
            config.trace_dir = Some(self.paths.trace_dir.to_string_lossy().to_string());
        }
        if config.plugins_dir.is_none() {
            config.plugins_dir = Some(self.paths.plugins_dir.to_string_lossy().to_string());
        }
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
        let _lifecycle = self.lifecycle.lock().await;
        let handle = self.gateway.lock().unwrap().take();
        if let Some(h) = handle {
            let _ = h.shutdown.send(());
            let _ = h.task.await;
        }
        Ok(self.status())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    fn temp_paths(tag: &str) -> (AppPaths, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "moonbridge-state-{}-{tag}",
            std::process::id()
        ));
        let paths = AppPaths::resolve(dir.join("config"), dir.join("data"));
        (paths, dir)
    }

    /// 回归：网关已在运行时再次 `start_gateway` 必须幂等返回，不得死锁。
    ///
    /// 历史缺陷：`if let Some(h) = self.gateway.lock()...` 仍持守卫时调用 `status()`，
    /// 再取同一把非重入 `std::sync::Mutex` ⇒ 永久阻塞。
    ///
    /// 为什么把被测调用放进独立 OS 线程 + 自有 runtime：死锁发生在同步 `lock()` 里，
    /// 若按直觉写 `timeout(dur, st.start_gateway())`，定时器与该 future 同属一个任务，
    /// 任务一旦阻塞就再也不能轮询定时器 —— 回归时表现为**测试挂死**而非失败。
    /// 现在阻塞只会泄漏那个分离线程，主测试线程靠 `recv_timeout` 干净地判定失败。
    #[test]
    fn start_gateway_while_running_does_not_deadlock() {
        let (paths, dir) = temp_paths("idempotent");
        let st = ManagedState::new(paths).unwrap();
        // 端口 0：交给内核分配，避免与真实网关(38440)或其它测试抢端口
        st.update_config(|c| c.gateway.addr = "127.0.0.1:0".to_string())
            .unwrap();

        let (tx, rx) = mpsc::channel::<Vec<Result<String, String>>>();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("测试 runtime 构建失败");
            let steps = rt.block_on(async {
                let mut out = Vec::new();
                out.push(
                    st.start_gateway()
                        .await
                        .map(|s| format!("首次启动 running={}", s.running))
                        .map_err(|e| format!("首次启动失败: {e}")),
                );
                out.push(
                    st.start_gateway()
                        .await
                        .map(|s| format!("重复启动 running={}", s.running))
                        .map_err(|e| format!("重复启动失败: {e}")),
                );
                out.push(Ok(format!("status running={}", st.status().running)));
                out
            });
            let _ = tx.send(steps);
        });

        let steps = rx
            .recv_timeout(Duration::from_secs(60))
            .expect("重复 start_gateway 死锁：工作线程 60s 内未返回（修复前必现）");
        assert_eq!(
            steps,
            vec![
                Ok("首次启动 running=true".to_string()),
                Ok("重复启动 running=true".to_string()),
                Ok("status running=true".to_string()),
            ],
            "已运行时再次 start 应幂等返回，且 status 仍可用"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 未启动时 running=false 且 addr 回落到配置值。
    #[test]
    fn status_falls_back_to_config_addr_when_stopped() {
        let (paths, dir) = temp_paths("fallback");
        let st = ManagedState::new(paths).unwrap();
        let status = st.status();
        assert!(!status.running);
        assert_eq!(
            status.addr, "127.0.0.1:38440",
            "未启动时应回落到配置里的监听地址"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
