//! 服务端启动参数解析，管理凭据与网关凭据独立。

use std::path::PathBuf;

pub const ADMIN_TOKEN_ENV: &str = "MOONBRIDGE_ADMIN_TOKEN";
pub const GATEWAY_TOKEN_ENV: &str = "MOONBRIDGE_GATEWAY_TOKEN";
pub const KEY_FILE_ENV: &str = "MOONBRIDGE_KEY_FILE";
pub const WEB_DIR_ENV: &str = "MOONBRIDGE_WEB_DIR";
pub const DEFAULT_WEB_DIR: &str = "./ui/dist";
pub const APP_DIR_NAME: &str = "com.moonbridge.next";

#[derive(Clone, PartialEq, Eq)]
pub struct ServerOpts {
    pub admin_token: String,
    pub gateway_token: Option<String>,
    pub key_file: Option<PathBuf>,
    pub addr: Option<String>,
    pub config_dir: Option<PathBuf>,
    pub data_dir: Option<PathBuf>,
    pub web_dir: PathBuf,
}

impl std::fmt::Debug for ServerOpts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerOpts")
            .field("admin_token", &"[REDACTED]")
            .field(
                "gateway_token",
                &self.gateway_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("key_file", &self.key_file)
            .field("addr", &self.addr)
            .field("config_dir", &self.config_dir)
            .field("data_dir", &self.data_dir)
            .field("web_dir", &self.web_dir)
            .finish()
    }
}

pub fn parse_args(args: &[String]) -> std::result::Result<ServerOpts, String> {
    parse_args_with_env(
        args,
        std::env::var(ADMIN_TOKEN_ENV).ok(),
        std::env::var(WEB_DIR_ENV).ok(),
        std::env::var(GATEWAY_TOKEN_ENV).ok(),
        std::env::var(KEY_FILE_ENV).ok(),
    )
}

fn parse_args_with_env(
    args: &[String],
    env_token: Option<String>,
    env_web_dir: Option<String>,
    env_gateway_token: Option<String>,
    env_key_file: Option<String>,
) -> std::result::Result<ServerOpts, String> {
    let mut admin_token = None;
    let mut gateway_token = None;
    let mut key_file = None;
    let mut addr = None;
    let mut config_dir = None;
    let mut data_dir = None;
    let mut web_dir = None;

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
        } else if arg == "--gateway-token" || arg.starts_with("--gateway-token=") {
            gateway_token = Some(take_value("--gateway-token", arg, &mut rest)?);
        } else if arg == "--key-file" || arg.starts_with("--key-file=") {
            key_file = Some(PathBuf::from(take_value("--key-file", arg, &mut rest)?));
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
            return Err("未知参数，用法见 --help".to_string());
        } else {
            return Err("不支持位置参数，用法见 --help".to_string());
        }
    }

    let admin_token = admin_token
        .or(env_token)
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .ok_or_else(|| {
            format!("必须显式传入 admin token：--admin-token <TOKEN> 或设置 {ADMIN_TOKEN_ENV}")
        })?;
    if !moonbridge_gateway::auth::token_is_valid(&admin_token) {
        return Err("admin token 必须是非空且不含空白的可打印 ASCII 字符".to_string());
    }
    let gateway_token = gateway_token
        .or(env_gateway_token)
        .map(|t| t.trim().to_string());
    if let Some(token) = gateway_token.as_ref() {
        if !moonbridge_gateway::auth::token_is_valid(token) || token == &admin_token {
            return Err("gateway token 必须非空且不同于 admin token".to_string());
        }
    }
    let key_file = key_file.or_else(|| env_key_file.map(PathBuf::from));
    let web_dir = web_dir
        .or_else(|| env_web_dir.map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from(DEFAULT_WEB_DIR));

    Ok(ServerOpts {
        admin_token,
        gateway_token,
        key_file,
        addr,
        config_dir,
        data_dir,
        web_dir,
    })
}

pub fn usage() -> &'static str {
    "用法: moonbridge-server --admin-token <TOKEN> [选项]\n\
     \n\
     Moon Bridge Next 服务端：同一端口提供 LLM 网关、管理 API 与前端静态托管。\n\
     \n\
     必需:\n  \
     --admin-token <TOKEN>  /api/* 管理认证（或设置 MOONBRIDGE_ADMIN_TOKEN）\n\
     \n\
     可选:\n  \
     --gateway-token <TOKEN> 网关认证（或设置 MOONBRIDGE_GATEWAY_TOKEN，否则使用配置）\n  \
     --key-file <FILE>      主密钥文件（或设置 MOONBRIDGE_KEY_FILE）\n  \
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

    fn parse(v: &[&str]) -> Result<ServerOpts, String> {
        parse_args_with_env(&args(v), None, None, None, None)
    }

    #[test]
    fn parses_full_options() {
        let opts = parse(&[
            "--admin-token",
            "sekret",
            "--gateway-token",
            "gateway-secret",
            "--key-file=/srv/keys/master.key",
            "--addr",
            "127.0.0.1:9999",
            "--config-dir=/tmp/cfg",
            "--data-dir",
            "/tmp/data",
            "--web-dir",
            "/srv/ui/dist",
        ])
        .unwrap();
        assert_eq!(
            opts,
            ServerOpts {
                admin_token: "sekret".into(),
                gateway_token: Some("gateway-secret".into()),
                key_file: Some(PathBuf::from("/srv/keys/master.key")),
                addr: Some("127.0.0.1:9999".into()),
                config_dir: Some(PathBuf::from("/tmp/cfg")),
                data_dir: Some(PathBuf::from("/tmp/data")),
                web_dir: PathBuf::from("/srv/ui/dist"),
            }
        );
    }

    #[test]
    fn token_env_is_fallback_and_flag_wins() {
        let opts =
            parse_args_with_env(&args(&[]), Some("env-token".into()), None, None, None).unwrap();
        assert_eq!(opts.admin_token, "env-token");
        assert!(opts.addr.is_none());
        assert!(opts.config_dir.is_none());
        assert!(opts.data_dir.is_none());
        assert!(opts.gateway_token.is_none());
        assert!(opts.key_file.is_none());
        let opts = parse_args_with_env(
            &args(&["--admin-token=flag-token"]),
            Some("env-token".into()),
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(opts.admin_token, "flag-token", "显式传参优先于环境变量");
    }

    #[test]
    fn gateway_token_and_key_file_env_are_fallbacks_and_flags_win() {
        let opts = parse_args_with_env(
            &args(&["--admin-token=admin"]),
            None,
            None,
            Some(" env-gateway ".into()),
            Some("/env/master.key".into()),
        )
        .unwrap();
        assert_eq!(opts.gateway_token.as_deref(), Some("env-gateway"));
        assert_eq!(opts.key_file, Some(PathBuf::from("/env/master.key")));
        let opts = parse_args_with_env(
            &args(&[
                "--admin-token=admin",
                "--gateway-token=flag-gateway",
                "--key-file",
                "/flag/master.key",
            ]),
            None,
            None,
            Some("env-gateway".into()),
            Some("/env/master.key".into()),
        )
        .unwrap();
        assert_eq!(opts.gateway_token.as_deref(), Some("flag-gateway"));
        assert_eq!(opts.key_file, Some(PathBuf::from("/flag/master.key")));
    }

    #[test]
    fn web_dir_env_is_fallback_and_flag_wins() {
        let opts = parse_args_with_env(
            &args(&["--admin-token", "t"]),
            None,
            Some("/env/dist".into()),
            None,
            None,
        )
        .unwrap();
        assert_eq!(opts.web_dir, PathBuf::from("/env/dist"));
        let opts = parse_args_with_env(
            &args(&["--admin-token", "t", "--web-dir=/flag/dist"]),
            None,
            Some("/env/dist".into()),
            None,
            None,
        )
        .unwrap();
        assert_eq!(opts.web_dir, PathBuf::from("/flag/dist"));
        assert_eq!(
            parse(&["--admin-token", "t"]).unwrap().web_dir,
            PathBuf::from(DEFAULT_WEB_DIR)
        );
    }

    #[test]
    fn missing_token_is_rejected() {
        let err = parse(&[]).unwrap_err();
        assert!(err.contains("--admin-token"), "应提示显式传入: {err}");
        assert!(err.contains(ADMIN_TOKEN_ENV), "应提示环境变量回退: {err}");
        let err = parse(&["--admin-token", "  "]).unwrap_err();
        assert!(err.contains("--admin-token"), "空白 token 同样拒绝: {err}");
        let err =
            parse_args_with_env(&args(&[]), Some("   ".into()), None, None, None).unwrap_err();
        assert!(
            err.contains("--admin-token"),
            "空白 env token 同样拒绝: {err}"
        );
    }

    #[test]
    fn gateway_token_must_be_nonempty_and_different_from_admin() {
        for token in ["", "   ", "admin-secret", " admin-secret "] {
            assert!(parse_args_with_env(
                &args(&["--admin-token=admin-secret"]),
                None,
                None,
                Some(token.into()),
                None,
            )
            .is_err());
            assert!(
                parse_args_with_env(
                    &args(&["--admin-token=admin-secret", "--gateway-token", token]),
                    None,
                    None,
                    Some("valid-fallback".into()),
                    None,
                )
                .is_err(),
                "显式无效值不能回退到环境变量"
            );
        }
        assert!(parse(&["--admin-token=admin", "--gateway-token="]).is_err());
    }

    #[test]
    fn invalid_tokens_are_rejected_without_disclosing_secrets() {
        for token in [
            "private secret",
            "private\tsecret",
            "private\nsecret",
            "private\rsecret",
            "private\0secret",
            "private\u{1f}secret",
            "private\u{7f}secret",
            "private令牌secret",
            "private\u{a0}secret",
        ] {
            let admin_flag = format!("--admin-token={token}");
            let gateway_flag = format!("--gateway-token={token}");
            for result in [
                parse(&["--admin-token", token]),
                parse(&[&admin_flag]),
                parse_args_with_env(&args(&[]), Some(token.into()), None, None, None),
                parse(&["--admin-token=admin-safe", "--gateway-token", token]),
                parse(&["--admin-token=admin-safe", &gateway_flag]),
                parse_args_with_env(
                    &args(&["--admin-token=admin-safe"]),
                    None,
                    None,
                    Some(token.into()),
                    None,
                ),
            ] {
                let err = result.unwrap_err();
                assert!(!err.contains(token));
                assert!(!err.contains("private"));
                assert!(!err.contains("secret"));
                assert!(!err.contains("admin-safe"));
            }
        }
        for token in ["safe-AZaz09", "safe-._~+/=!@#$%^&*()[]{}:;\"'<>?,\\|"] {
            let opts = parse(&["--admin-token", token, "--gateway-token=gateway-safe"]).unwrap();
            assert_eq!(opts.admin_token, token);
            let opts = parse_args_with_env(
                &args(&[]),
                Some("admin-safe".into()),
                None,
                Some(token.into()),
                None,
            )
            .unwrap();
            assert_eq!(opts.gateway_token.as_deref(), Some(token));
        }
    }

    #[test]
    fn debug_and_errors_do_not_disclose_credentials() {
        let opts = parse(&[
            "--admin-token=admin-private-value",
            "--gateway-token=gateway-private-value",
        ])
        .unwrap();
        let debug = format!("{opts:?}");
        assert!(!debug.contains("admin-private-value"));
        assert!(!debug.contains("gateway-private-value"));
        let err = parse(&[
            "--admin-token=same-private-value",
            "--gateway-token=same-private-value",
        ])
        .unwrap_err();
        assert!(!err.contains("same-private-value"));
        let err = parse(&[
            "--admin-token=admin-private-value",
            "--unknown=gateway-private-value",
        ])
        .unwrap_err();
        assert!(!err.contains("admin-private-value"));
        assert!(!err.contains("gateway-private-value"));
    }

    #[test]
    fn unknown_and_positional_args_are_rejected() {
        let err = parse(&["--admin-token", "t", "--port", "1"]).unwrap_err();
        assert!(err.contains("未知参数"), "{err}");
        let err = parse(&["--admin-token", "t", "serve"]).unwrap_err();
        assert!(err.contains("不支持位置参数"), "{err}");
        for flag in ["--addr", "--gateway-token", "--key-file"] {
            assert!(parse(&["--admin-token", "t", flag])
                .unwrap_err()
                .contains("缺少取值"));
            let empty = format!("{flag}=");
            assert!(parse(&["--admin-token", "t", &empty])
                .unwrap_err()
                .contains("的值不能为空"));
        }
    }

    #[test]
    fn help_returns_usage_text() {
        for flag in ["--help", "-h"] {
            let err = parse(&[flag]).unwrap_err();
            assert!(err.starts_with("用法: moonbridge-server"), "{err}");
            assert!(err.contains("--admin-token"), "{err}");
        }
        let text = usage();
        for flag in ["--web-dir", "--data-dir", "--gateway-token", "--key-file"] {
            assert!(text.contains(flag), "{text}");
        }
    }

    #[test]
    fn public_parse_args_reads_process_env() {
        let opts = parse_args(&args(&[
            "--admin-token=flag-token",
            "--gateway-token=flag-gateway",
            "--key-file=/flag/master.key",
        ]))
        .unwrap();
        assert_eq!(opts.admin_token, "flag-token");
        assert_eq!(opts.gateway_token.as_deref(), Some("flag-gateway"));
        assert_eq!(opts.key_file, Some(PathBuf::from("/flag/master.key")));
    }
}
