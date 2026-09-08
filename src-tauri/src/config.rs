//! 引导配置：网关运行参数 + 应用级设置，持久化于 `app_config_dir/config.toml`。
//!
//! 与 SQLite 业务配置分离——这里只放「如何启动网关 / 日志 / 路径」等运行时参数。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use moonbridge_gateway::GatewayConfig;
use serde::{Deserialize, Serialize};

/// 应用关键路径集合（由 Tauri 的 config/data 目录派生）。
#[derive(Debug, Clone)]
pub struct AppPaths {
    /// 配置目录（`app_config_dir`）。
    pub config_dir: PathBuf,
    /// 数据目录（`app_data_dir`）。
    pub data_dir: PathBuf,
    /// 引导配置文件：`config_dir/config.toml`。
    pub config_file: PathBuf,
    /// SQLite 数据库：`data_dir/moonbridge.db`。
    pub db_path: PathBuf,
    /// trace 落盘目录：`data_dir/traces`。
    pub trace_dir: PathBuf,
    /// 用户插件目录：`data_dir/plugins`。
    pub plugins_dir: PathBuf,
}

impl AppPaths {
    /// 由 Tauri 提供的配置/数据目录派生全部路径。
    pub fn resolve(config_dir: PathBuf, data_dir: PathBuf) -> Self {
        Self {
            config_file: config_dir.join("config.toml"),
            db_path: data_dir.join("moonbridge.db"),
            trace_dir: data_dir.join("traces"),
            plugins_dir: data_dir.join("plugins"),
            config_dir,
            data_dir,
        }
    }

    /// 确保数据/插件/trace 目录存在。
    pub fn ensure_dirs(&self) -> Result<()> {
        std::fs::create_dir_all(&self.config_dir).context("创建配置目录失败")?;
        std::fs::create_dir_all(&self.data_dir).context("创建数据目录失败")?;
        std::fs::create_dir_all(&self.plugins_dir).context("创建插件目录失败")?;
        std::fs::create_dir_all(&self.trace_dir).context("创建 trace 目录失败")?;
        Ok(())
    }
}

/// 应用引导配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppConfig {
    /// 网关运行参数。
    #[serde(default)]
    pub gateway: GatewayConfig,
    /// 日志级别（trace/debug/info/warn/error）。
    #[serde(default = "default_log_level")]
    pub log_level: String,
    /// 应用启动时是否自动开启网关。
    #[serde(default = "default_true")]
    pub auto_start: bool,
}

fn default_log_level() -> String {
    "info".to_string()
}
fn default_true() -> bool {
    true
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            gateway: GatewayConfig::default(),
            log_level: default_log_level(),
            auto_start: true,
        }
    }
}

impl AppConfig {
    /// 从文件加载；文件不存在时返回默认配置（不报错）。
    pub fn load_or_default(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("读取配置文件失败: {}", path.display()))?;
        let cfg: AppConfig = toml::from_str(&text)
            .with_context(|| format!("解析配置文件失败: {}", path.display()))?;
        Ok(cfg)
    }

    /// 保存到文件（覆盖写，父目录须已存在）。
    pub fn save(&self, path: &Path) -> Result<()> {
        let text = toml::to_string_pretty(self).context("序列化配置失败")?;
        std::fs::write(path, text).with_context(|| format!("写入配置文件失败: {}", path.display()))?;
        Ok(())
    }
}
