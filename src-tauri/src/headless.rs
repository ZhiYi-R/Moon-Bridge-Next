//! 无头模式：不依赖桌面环境，仅提供带鉴权的网关服务。
//!
//! 由 `--headless` 启动参数进入（见 `main.rs`），全程不初始化 Tauri/WebView，
//! 只做：解析参数 → 打开数据库 → [`moonbridge_gateway::bootstrap`] → 阻塞服务，
//! `Ctrl-C` 优雅退出。
//!
//! 鉴权强制开启：必须显式传入 admin token（`--admin-token` 或环境变量
//! `MOONBRIDGE_ADMIN_TOKEN`），缺失时拒绝启动，绝不以无鉴权状态监听。
//! 命令行传入的值覆盖配置文件中的 `auth_token`，且只驻留内存、永不回写文件。

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use tracing_subscriber::EnvFilter;

use crate::config::{AppConfig, AppPaths};

/// 与 Tauri `identifier`（见 `tauri.conf.json`）对应的目录名。headless 默认复用
/// 桌面端同一套数据库与插件（Linux 上即 `~/.config/com.moonbridge.next` 与
/// `~/.local/share/com.moonbridge.next`）。
const APP_DIR_NAME: &str = "com.moonbridge.next";

/// admin token 的环境变量回退（与 `--admin-token` 等价，显式传参优先）。
const ADMIN_TOKEN_ENV: &str = "MOONBRIDGE_ADMIN_TOKEN";

/// headless 运行选项（`main.rs` 手工解析后传入）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadlessOpts {
    /// 入口 Bearer token（强制，覆盖配置文件）。
    pub admin_token: String,
    /// 监听地址覆盖（如 `127.0.0.1:38440`）；`None` 时用配置文件。
    pub addr: Option<String>,
    /// 配置目录覆盖；`None` 时用平台默认。
    pub config_dir: Option<PathBuf>,
    /// 数据目录覆盖；`None` 时用平台默认。
    pub data_dir: Option<PathBuf>,
}

/// 解析 headless 参数。`args` 为程序名之后的全量参数（含 `--headless` 本体，
/// 解析时跳过）。未知长选项与位置参数一律报错，避免拼写错误被静默忽略。
pub fn parse_args(args: &[String]) -> std::result::Result<HeadlessOpts, String> {
    parse_args_with_env(args, std::env::var(ADMIN_TOKEN_ENV).ok())
}

/// `parse_args` 的可测试内核：`env_token` 为环境变量回退的显式注入。
fn parse_args_with_env(
    args: &[String],
    env_token: Option<String>,
) -> std::result::Result<HeadlessOpts, String> {
    let mut admin_token: Option<String> = None;
    let mut addr: Option<String> = None;
    let mut config_dir: Option<PathBuf> = None;
    let mut data_dir: Option<PathBuf> = None;

    // `--key value` 与 `--key=value` 两种写法都接受。
    fn take_value(
        flag: &str,
        arg: &str,
        rest: &mut std::slice::Iter<String>,
    ) -> std::result::Result<String, String> {
        if let Some(v) = arg.split_once('=').map(|(_, v)| v.to_string()) {
            if v.is_empty() {
                return Err(format!("{flag} 的值不能为空"));
            }
            return Ok(v);
        }
        rest.next()
            .cloned()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| format!("{flag} 缺少取值"))
    }

    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        if arg == "--headless" {
            continue;
        } else if arg == "--admin-token" || arg.starts_with("--admin-token=") {
            admin_token = Some(take_value("--admin-token", arg, &mut rest)?);
        } else if arg == "--addr" || arg.starts_with("--addr=") {
            addr = Some(take_value("--addr", arg, &mut rest)?);
        } else if arg == "--config-dir" || arg.starts_with("--config-dir=") {
            config_dir = Some(PathBuf::from(take_value("--config-dir", arg, &mut rest)?));
        } else if arg == "--data-dir" || arg.starts_with("--data-dir=") {
            data_dir = Some(PathBuf::from(take_value("--data-dir", arg, &mut rest)?));
        } else if arg == "--help" || arg == "-h" {
            return Err(usage().to_string());
        } else if arg.starts_with('-') {
            return Err(format!("未知参数 {arg}，用法见 --headless --help"));
        } else {
            return Err(format!("不支持位置参数 {arg}，用法见 --headless --help"));
        }
    }

    let admin_token = admin_token
        .or(env_token)
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .ok_or_else(|| {
            format!(
                "headless 模式必须显式传入 admin token：--admin-token <TOKEN> 或设置 {ADMIN_TOKEN_ENV}"
            )
        })?;

    Ok(HeadlessOpts {
        admin_token,
        addr,
        config_dir,
        data_dir,
    })
}

/// headless 用法说明。
pub fn usage() -> &'static str {
    "用法: moonbridge-app --headless --admin-token <TOKEN> [选项]\n\
     \n\
     无头模式：不依赖桌面环境，仅启动带鉴权的网关服务。\n\
     \n\
     必需:\n  \
     --admin-token <TOKEN>  入口 Bearer token（或设置 MOONBRIDGE_ADMIN_TOKEN）\n\
     \n\
     可选:\n  \
     --addr <ADDR>          监听地址覆盖，如 127.0.0.1:38440\n  \
     --config-dir <DIR>     配置目录（默认与桌面端同一位置）\n  \
     --data-dir <DIR>       数据目录，含数据库与插件（默认与桌面端同一位置）\n  \
     --help                 显示本帮助"
}

/// 运行无头网关（阻塞到 `Ctrl-C`）。token 只驻留内存，不回写配置文件。
pub async fn run_headless(opts: HeadlessOpts) -> Result<()> {
    if opts.admin_token.trim().is_empty() {
        anyhow::bail!("headless 模式必须显式传入非空 admin token");
    }

    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .finish();
    let _ = tracing::subscriber::set_global_default(subscriber);

    let config_dir = opts
        .config_dir
        .clone()
        .or_else(|| dirs::config_dir().map(|d| d.join(APP_DIR_NAME)));
    let data_dir = opts
        .data_dir
        .clone()
        .or_else(|| dirs::data_dir().map(|d| d.join(APP_DIR_NAME)));
    let (Some(config_dir), Some(data_dir)) = (config_dir, data_dir) else {
        anyhow::bail!("无法确定平台默认目录，请显式传入 --config-dir 与 --data-dir");
    };

    let paths = AppPaths::resolve(config_dir, data_dir);
    paths.ensure_dirs().context("初始化应用目录失败")?;
    let mut app_config = AppConfig::load_or_default(&paths.config_file).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "引导配置加载失败，使用默认配置");
        AppConfig::default()
    });
    if let Some(addr) = opts.addr {
        app_config.gateway.addr = addr;
    }
    // 显式传入优先：覆盖配置文件，且不回写（日志里也绝不打印 token 本体）。
    app_config.gateway.auth_token = Some(opts.admin_token);
    if app_config.gateway.trace_dir.is_none() {
        app_config.gateway.trace_dir = Some(paths.trace_dir.to_string_lossy().to_string());
    }
    if app_config.gateway.plugins_dir.is_none() {
        app_config.gateway.plugins_dir = Some(paths.plugins_dir.to_string_lossy().to_string());
    }

    let db = moonbridge_store::Database::open(&paths.db_path)
        .with_context(|| format!("打开数据库失败: {}", paths.db_path.display()))?;
    let state = moonbridge_gateway::bootstrap(app_config.gateway.clone(), Arc::new(db))
        .context("构建网关状态失败")?;
    tracing::info!(addr = %state.config.addr, "headless 网关已启动（鉴权已开启），Ctrl-C 退出");
    let shutdown = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    moonbridge_gateway::server::serve_with_shutdown(state, shutdown).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_full_options() {
        let opts = parse_args_with_env(
            &args(&[
                "--headless",
                "--admin-token",
                "sekret",
                "--addr",
                "127.0.0.1:9999",
                "--config-dir=/tmp/cfg",
                "--data-dir",
                "/tmp/data",
            ]),
            None,
        )
        .unwrap();
        assert_eq!(
            opts,
            HeadlessOpts {
                admin_token: "sekret".into(),
                addr: Some("127.0.0.1:9999".into()),
                config_dir: Some(PathBuf::from("/tmp/cfg")),
                data_dir: Some(PathBuf::from("/tmp/data")),
            }
        );
    }

    #[test]
    fn env_token_is_fallback_and_flag_wins() {
        let opts = parse_args_with_env(&args(&["--headless"]), Some("env-token".into())).unwrap();
        assert_eq!(opts.admin_token, "env-token");
        assert!(opts.addr.is_none());

        let opts = parse_args_with_env(
            &args(&["--headless", "--admin-token=flag-token"]),
            Some("env-token".into()),
        )
        .unwrap();
        assert_eq!(opts.admin_token, "flag-token");
    }

    #[test]
    fn missing_token_is_rejected() {
        let err = parse_args_with_env(&args(&["--headless"]), None).unwrap_err();
        assert!(err.contains("--admin-token"), "应提示显式传入: {err}");

        let err =
            parse_args_with_env(&args(&["--headless", "--admin-token", "  "]), None).unwrap_err();
        assert!(err.contains("--admin-token"), "空白 token 同样拒绝: {err}");
    }

    #[test]
    fn unknown_and_positional_args_are_rejected() {
        assert!(parse_args_with_env(&args(&["--headless", "--port", "1"]), None).is_err());
        assert!(parse_args_with_env(&args(&["--headless", "serve"]), None).is_err());
        assert!(parse_args_with_env(&args(&["--headless", "--addr"]), None)
            .unwrap_err()
            .contains("缺少取值"));
    }
}
