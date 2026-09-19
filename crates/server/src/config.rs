//! 引导配置：网关运行参数 + 应用级设置，持久化于 `config_dir/config.toml`。
//!
//! 与桌面端（`src-tauri/src/config.rs`）保持同一份文件格式与字段语义，使得同一个
//! 数据目录既可由桌面壳读取，也可由服务端读取；服务端不写任何新字段。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use moonbridge_gateway::GatewayConfig;
use serde::{Deserialize, Serialize};

/// 应用关键路径集合（由 `--config-dir` / `--data-dir` 派生）。
#[derive(Debug, Clone)]
pub struct AppPaths {
    /// 配置目录。
    pub config_dir: PathBuf,
    /// 数据目录。
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
    /// 由配置/数据目录派生全部路径。
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
    /// 应用启动时是否自动开启网关（服务端恒为常驻，字段仅为格式兼容）。
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
        let cfg: AppConfig =
            toml::from_str(&text).map_err(|_| anyhow::anyhow!("解析配置文件失败"))?;
        Ok(cfg)
    }

    /// 保存到文件（覆盖写，父目录须已存在）。
    pub fn save(&self, path: &Path) -> Result<()> {
        let text = toml::to_string_pretty(self).context("序列化配置失败")?;
        std::fs::write(path, text)
            .with_context(|| format!("写入配置文件失败: {}", path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 进程 pid 隔离的临时目录（不引入 tempfile 依赖，与桌面端测试同一模式）。
    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "moonbridge-server-config-{}-{tag}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn default_matches_declared_field_defaults() {
        let cfg = AppConfig::default();
        assert_eq!(cfg.log_level, "info");
        assert!(cfg.auto_start);
        assert_eq!(cfg.gateway.addr, "127.0.0.1:38440");
        assert!(cfg.gateway.auth_token.is_none(), "默认不带凭据");
        assert!(cfg.gateway.trace_dir.is_none());
    }

    #[test]
    fn resolve_derives_paths_from_dirs() {
        let paths = AppPaths::resolve(PathBuf::from("/cfg"), PathBuf::from("/data"));
        assert_eq!(paths.config_file, PathBuf::from("/cfg/config.toml"));
        assert_eq!(paths.db_path, PathBuf::from("/data/moonbridge.db"));
        assert_eq!(paths.trace_dir, PathBuf::from("/data/traces"));
        assert_eq!(paths.plugins_dir, PathBuf::from("/data/plugins"));
        assert!(!paths.db_path.exists());
    }

    #[test]
    fn ensure_dirs_creates_all_directories() {
        let dir = temp_dir("ensure");
        let paths = AppPaths::resolve(dir.join("cfg"), dir.join("data"));
        paths.ensure_dirs().unwrap();
        assert!(paths.config_dir.is_dir());
        assert!(paths.data_dir.is_dir());
        assert!(paths.plugins_dir.is_dir());
        assert!(paths.trace_dir.is_dir());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_or_default_falls_back_when_missing() {
        let dir = temp_dir("load-missing");
        let missing = dir.join("config.toml");
        let cfg = AppConfig::load_or_default(&missing).unwrap();
        assert_eq!(cfg.log_level, "info");
        assert!(cfg.auto_start);
        assert_eq!(cfg.gateway.addr, "127.0.0.1:38440");
        // 不存在的文件不应被顺手创建
        assert!(!missing.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = temp_dir("round-trip");
        let file = dir.join("config.toml");
        let mut cfg = AppConfig {
            log_level: "debug".to_string(),
            auto_start: false,
            ..Default::default()
        };
        cfg.gateway.addr = "0.0.0.0:8080".to_string();
        cfg.gateway.auth_token = Some("boot-token".to_string());
        cfg.gateway.plugins_dir = Some("/data/plugins".to_string());
        cfg.save(&file).unwrap();

        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains("logLevel"), "落盘为 camelCase: {text}");
        assert!(text.contains("authToken"), "{text}");

        let loaded = AppConfig::load_or_default(&file).unwrap();
        assert_eq!(loaded.log_level, "debug");
        assert!(!loaded.auto_start);
        assert_eq!(loaded.gateway.addr, "0.0.0.0:8080");
        assert_eq!(loaded.gateway.auth_token.as_deref(), Some("boot-token"));
        assert_eq!(loaded.gateway.plugins_dir.as_deref(), Some("/data/plugins"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_or_default_reports_broken_toml() {
        let dir = temp_dir("broken");
        let file = dir.join("config.toml");
        std::fs::write(&file, "this is not toml =").unwrap();
        let err = AppConfig::load_or_default(&file).unwrap_err();
        assert!(
            format!("{err:#}").contains("解析配置文件失败"),
            "应带上下文: {err:#}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn partial_toml_fills_missing_fields_with_defaults() {
        let dir = temp_dir("partial");
        let file = dir.join("config.toml");
        // 仅写一个字段：其余字段应回落到声明的默认值（配置向后兼容的关键）。
        std::fs::write(&file, "[gateway]\naddr = \"1.2.3.4:1\"\n").unwrap();
        let cfg = AppConfig::load_or_default(&file).unwrap();
        assert_eq!(cfg.gateway.addr, "1.2.3.4:1");
        assert_eq!(cfg.log_level, "info");
        assert!(cfg.auto_start);
        assert!(cfg.gateway.session_marker, "网关字段缺省即默认开启");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
