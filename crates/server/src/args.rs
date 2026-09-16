//! 服务端启动参数解析。
//!
//! 语义对齐桌面端的 headless 模式（`src-tauri/src/headless.rs`）：
//! - `--admin-token` / `MOONBRIDGE_ADMIN_TOKEN` **强制**提供，缺失拒绝启动；
//!   该 token 同时作为入口 Bearer token 覆盖配置文件的 `auth_token`，只驻留内存、
//!   永不回写、日志里绝不打印。
//! - `--addr` / `--config-dir` / `--data-dir` 与 headless 一致。
//! - 新增 `--web-dir` / `MOONBRIDGE_WEB_DIR`：前端静态文件目录（默认 `./ui/dist`）。
//! - 未知长选项与位置参数一律报错；`--key value` 与 `--key=value` 都接受。

use std::path::PathBuf;

/// admin token 的环境变量回退（与 `--admin-token` 等价，显式传参优先）。
pub const ADMIN_TOKEN_ENV: &str = "MOONBRIDGE_ADMIN_TOKEN";

/// 前端静态目录的环境变量回退（与 `--web-dir` 等价，显式传参优先）。
pub const WEB_DIR_ENV: &str = "MOONBRIDGE_WEB_DIR";

/// 前端静态文件目录默认值（相对进程 CWD）。
pub const DEFAULT_WEB_DIR: &str = "./ui/dist";

/// 与 Tauri `identifier`（见 `tauri.conf.json`）对应的目录名，用于平台默认目录。
pub const APP_DIR_NAME: &str = "com.moonbridge.next";

/// 服务端运行选项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerOpts {
    /// 入口 Bearer token（强制，覆盖配置文件）。
    pub admin_token: String,
    /// 监听地址覆盖（如 `127.0.0.1:38440`）；`None` 时用配置文件。
    pub addr: Option<String>,
    /// 配置目录覆盖；`None` 时用平台默认。
    pub config_dir: Option<PathBuf>,
    /// 数据目录覆盖；`None` 时用平台默认。
    pub data_dir: Option<PathBuf>,
    /// 前端静态文件目录。
    pub web_dir: PathBuf,
}

/// 解析启动参数（`args` 为程序名之后的全量参数）。
pub fn parse_args(args: &[String]) -> std::result::Result<ServerOpts, String> {
    parse_args_with_env(
        args,
        std::env::var(ADMIN_TOKEN_ENV).ok(),
        std::env::var(WEB_DIR_ENV).ok(),
    )
}

/// [`parse_args`] 的可测试内核：环境变量回退显式注入。
fn parse_args_with_env(
    args: &[String],
    env_token: Option<String>,
    env_web_dir: Option<String>,
) -> std::result::Result<ServerOpts, String> {
    let mut admin_token: Option<String> = None;
    let mut addr: Option<String> = None;
    let mut config_dir: Option<PathBuf> = None;
    let mut data_dir: Option<PathBuf> = None;
    let mut web_dir: Option<PathBuf> = None;

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
        if arg == "--admin-token" || arg.starts_with("--admin-token=") {
            admin_token = Some(take_value("--admin-token", arg, &mut rest)?);
        } else if arg == "--addr" || arg.starts_with("--addr=") {
            addr = Some(take_value("--addr", arg, &mut rest)?);
        } else if arg == "--config-dir" || arg.starts_with("--config-dir=") {
            config_dir = Some(PathBuf::from(take_value("--config-dir", arg, &mut rest)?));
        } else if arg == "--data-dir" || arg.starts_with("--data-dir=") {
            data_dir = Some(PathBuf::from(take_value("--data-dir", arg, &mut rest)?));
        } else if arg == "--web-dir" || arg.starts_with("--web-dir=") {
            web_dir = Some(PathBuf::from(take_value("--web-dir", arg, &mut rest)?));
        } else if arg == "--help" || arg == "-h" {
            return Err(usage().to_string());
        } else if arg.starts_with('-') {
            return Err(format!("未知参数 {arg}，用法见 --help"));
        } else {
            return Err(format!("不支持位置参数 {arg}，用法见 --help"));
        }
    }

    let admin_token = admin_token
        .or(env_token)
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .ok_or_else(|| {
            format!("必须显式传入 admin token：--admin-token <TOKEN> 或设置 {ADMIN_TOKEN_ENV}")
        })?;

    let web_dir = web_dir
        .or_else(|| env_web_dir.map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from(DEFAULT_WEB_DIR));

    Ok(ServerOpts {
        admin_token,
        addr,
        config_dir,
        data_dir,
        web_dir,
    })
}

/// 用法说明。
pub fn usage() -> &'static str {
    "用法: moonbridge-server --admin-token <TOKEN> [选项]\n\
     \n\
     Moon Bridge Next 服务端：同一端口提供 LLM 网关、管理 API 与前端静态托管。\n\
     \n\
     必需:\n  \
     --admin-token <TOKEN>  入口 Bearer token，同时用于 /api/* 管理认证\n  \
                            （或设置 MOONBRIDGE_ADMIN_TOKEN）\n\
     \n\
     可选:\n  \
     --addr <ADDR>          监听地址覆盖，如 0.0.0.0:38440\n  \
     --config-dir <DIR>     配置目录（默认与桌面端同一位置）\n  \
     --data-dir <DIR>       数据目录，含数据库、插件与 trace（默认与桌面端同一位置）\n  \
     --web-dir <DIR>        前端静态文件目录（默认 ./ui/dist，或设置 MOONBRIDGE_WEB_DIR）\n  \
     --help                 显示本帮助"
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
                "--admin-token",
                "sekret",
                "--addr",
                "127.0.0.1:9999",
                "--config-dir=/tmp/cfg",
                "--data-dir",
                "/tmp/data",
                "--web-dir",
                "/srv/ui/dist",
            ]),
            None,
            None,
        )
        .unwrap();
        assert_eq!(
            opts,
            ServerOpts {
                admin_token: "sekret".into(),
                addr: Some("127.0.0.1:9999".into()),
                config_dir: Some(PathBuf::from("/tmp/cfg")),
                data_dir: Some(PathBuf::from("/tmp/data")),
                web_dir: PathBuf::from("/srv/ui/dist"),
            }
        );
    }

    #[test]
    fn token_env_is_fallback_and_flag_wins() {
        let opts = parse_args_with_env(&args(&[]), Some("env-token".into()), None).unwrap();
        assert_eq!(opts.admin_token, "env-token");
        assert!(opts.addr.is_none());
        assert!(opts.config_dir.is_none());
        assert!(opts.data_dir.is_none());

        let opts = parse_args_with_env(
            &args(&["--admin-token=flag-token"]),
            Some("env-token".into()),
            None,
        )
        .unwrap();
        assert_eq!(opts.admin_token, "flag-token", "显式传参优先于环境变量");
    }

    #[test]
    fn web_dir_env_is_fallback_and_flag_wins() {
        let opts = parse_args_with_env(
            &args(&["--admin-token", "t"]),
            None,
            Some("/env/dist".into()),
        )
        .unwrap();
        assert_eq!(opts.web_dir, PathBuf::from("/env/dist"));

        let opts = parse_args_with_env(
            &args(&["--admin-token", "t", "--web-dir=/flag/dist"]),
            None,
            Some("/env/dist".into()),
        )
        .unwrap();
        assert_eq!(opts.web_dir, PathBuf::from("/flag/dist"));

        let opts = parse_args_with_env(&args(&["--admin-token", "t"]), None, None).unwrap();
        assert_eq!(opts.web_dir, PathBuf::from(DEFAULT_WEB_DIR));
    }

    #[test]
    fn missing_token_is_rejected() {
        let err = parse_args_with_env(&args(&[]), None, None).unwrap_err();
        assert!(err.contains("--admin-token"), "应提示显式传入: {err}");
        assert!(err.contains(ADMIN_TOKEN_ENV), "应提示环境变量回退: {err}");

        let err = parse_args_with_env(&args(&["--admin-token", "  "]), None, None).unwrap_err();
        assert!(err.contains("--admin-token"), "空白 token 同样拒绝: {err}");

        let err = parse_args_with_env(&args(&[]), Some("   ".into()), None).unwrap_err();
        assert!(
            err.contains("--admin-token"),
            "空白 env token 同样拒绝: {err}"
        );
    }

    #[test]
    fn unknown_and_positional_args_are_rejected() {
        let err = parse_args_with_env(&args(&["--admin-token", "t", "--port", "1"]), None, None)
            .unwrap_err();
        assert!(err.contains("未知参数"), "{err}");
        let err =
            parse_args_with_env(&args(&["--admin-token", "t", "serve"]), None, None).unwrap_err();
        assert!(err.contains("不支持位置参数"), "{err}");
        assert!(
            parse_args_with_env(&args(&["--admin-token", "t", "--addr"]), None, None)
                .unwrap_err()
                .contains("缺少取值")
        );
        assert!(
            parse_args_with_env(&args(&["--admin-token", "t", "--addr="]), None, None)
                .unwrap_err()
                .contains("的值不能为空")
        );
    }

    #[test]
    fn help_returns_usage_text() {
        for flag in ["--help", "-h"] {
            let err = parse_args_with_env(&args(&[flag]), None, None).unwrap_err();
            assert!(err.starts_with("用法: moonbridge-server"), "{err}");
            assert!(err.contains("--admin-token"), "{err}");
        }
        let text = usage();
        assert!(
            text.contains("--web-dir") && text.contains("--data-dir"),
            "{text}"
        );
    }

    #[test]
    fn public_parse_args_reads_process_env() {
        // 显式传参优先级高于环境变量，因此该断言不受测试进程环境影响。
        let opts = parse_args(&args(&["--admin-token=flag-token"])).unwrap();
        assert_eq!(opts.admin_token, "flag-token");
    }
}
