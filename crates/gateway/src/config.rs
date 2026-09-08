//! 网关引导配置（监听地址、认证、egress 代理、配额等）。
//!
//! 与 SQLite 中的业务配置分离：这些是「如何启动网关」的运行时参数，由 app 层
//! 从 `app_config_dir/config.toml` 加载后传入。

use serde::{Deserialize, Serialize};

/// 网关运行配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayConfig {
    /// 监听地址，如 `127.0.0.1:38440`。
    #[serde(default = "default_addr")]
    pub addr: String,
    /// Bearer 认证 token；`None` 表示不鉴权。
    #[serde(default)]
    pub auth_token: Option<String>,
    /// 所有上游调用的出站代理（HTTP(S)）。
    #[serde(default)]
    pub egress_proxy: Option<String>,
    /// 报文层钩子处理的 body 上限（字节），超限降级为只读并跳过 Lua。
    #[serde(default = "default_max_body")]
    pub max_body_bytes: usize,
    /// 上游请求超时（秒）。LLM 请求通常较长。
    #[serde(default = "default_timeout")]
    pub request_timeout_secs: u64,
    /// trace 落盘目录；`None` 表示不落盘。
    #[serde(default)]
    pub trace_dir: Option<String>,
}

fn default_addr() -> String {
    "127.0.0.1:38440".to_string()
}
fn default_max_body() -> usize {
    8 * 1024 * 1024
}
fn default_timeout() -> u64 {
    300
}

impl Default for GatewayConfig {
    fn default() -> Self {
        GatewayConfig {
            addr: default_addr(),
            auth_token: None,
            egress_proxy: None,
            max_body_bytes: default_max_body(),
            request_timeout_secs: default_timeout(),
            trace_dir: None,
        }
    }
}

impl GatewayConfig {
    /// 从 TOML 文本解析（缺失字段用默认值）。
    pub fn from_toml(s: &str) -> Result<Self, toml::de::Error> {
        // 先填充默认，再覆盖，容忍部分字段缺失
        let mut cfg = GatewayConfig::default();
        let partial: PartialConfig = toml::from_str(s)?;
        if let Some(v) = partial.addr {
            cfg.addr = v;
        }
        if let Some(v) = partial.auth_token {
            cfg.auth_token = Some(v);
        }
        if let Some(v) = partial.egress_proxy {
            cfg.egress_proxy = Some(v);
        }
        if let Some(v) = partial.max_body_bytes {
            cfg.max_body_bytes = v;
        }
        if let Some(v) = partial.request_timeout_secs {
            cfg.request_timeout_secs = v;
        }
        if let Some(v) = partial.trace_dir {
            cfg.trace_dir = Some(v);
        }
        Ok(cfg)
    }
}

/// 部分字段配置（用于宽容解析 TOML）。
#[derive(Debug, Deserialize)]
struct PartialConfig {
    addr: Option<String>,
    auth_token: Option<String>,
    egress_proxy: Option<String>,
    max_body_bytes: Option<usize>,
    request_timeout_secs: Option<u64>,
    trace_dir: Option<String>,
}
