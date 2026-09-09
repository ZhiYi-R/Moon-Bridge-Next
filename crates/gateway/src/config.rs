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
    /// 插件脚本目录：`script_ref` 里的相对 `.lua` 路径归一到此处，绝对路径也必须
    /// 落在其中才允许加载（见 `crate::read_script`）。`None` 时退化为按进程 CWD 解析。
    #[serde(default)]
    pub plugins_dir: Option<String>,
    /// 上游请求超时（秒）。
    ///
    /// 注意：当前**仅**用于派生插件沙箱的 `call_timeout`（见 `lib.rs::sandbox_limits`）。
    /// 上游 HTTP 客户端刻意不设总超时，只设 30s 连接超时——LLM 流式响应可远超此值，
    /// 见 `upstream.rs::build_client`。故此值不影响上游请求本身。
    #[serde(default = "default_timeout")]
    pub request_timeout_secs: u64,
    /// trace 落盘目录；`None` 表示不落盘。
    #[serde(default)]
    pub trace_dir: Option<String>,
    /// 会话水印开关：把 `[mb:xxxxxx]` 附在助手**纯文本**输出末尾，靠客户端下一轮带回
    /// 来识别会话。入站会先剥净再转发上游，上游模型永远看不到它。
    ///
    /// 面向的是**不携带任何会话标识**的客户端（如 Qwen Code：其 `prompt_cache_key`
    /// 注入以直连 `api.openai.com` 为前提）。若你的客户端已通过 `session_id` 字段、
    /// `previous_response_id` 或 `X-Codex-Window-Id` 头表明身份，可关掉本开关。
    /// 代价：标记会出现在用户可见的回复末尾与本地 transcript 里，并每轮占约 5 token。
    /// 关掉后**仍会剥除**入站 marker（客户端可能带着开启期间留下的历史），只是不再追加。
    #[serde(default = "default_session_marker")]
    pub session_marker: bool,
    /// 活跃会话表的深度上限（FIFO，超深挤出最老者并连带清理其插件态）。
    #[serde(default = "default_session_depth")]
    pub session_table_depth: usize,
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
fn default_session_marker() -> bool {
    true
}
fn default_session_depth() -> usize {
    64
}

impl Default for GatewayConfig {
    fn default() -> Self {
        GatewayConfig {
            addr: default_addr(),
            auth_token: None,
            egress_proxy: None,
            max_body_bytes: default_max_body(),
            plugins_dir: None,
            request_timeout_secs: default_timeout(),
            trace_dir: None,
            session_marker: default_session_marker(),
            session_table_depth: default_session_depth(),
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
        if let Some(v) = partial.plugins_dir {
            cfg.plugins_dir = Some(v);
        }
        if let Some(v) = partial.request_timeout_secs {
            cfg.request_timeout_secs = v;
        }
        if let Some(v) = partial.trace_dir {
            cfg.trace_dir = Some(v);
        }
        if let Some(v) = partial.session_marker {
            cfg.session_marker = v;
        }
        if let Some(v) = partial.session_table_depth {
            cfg.session_table_depth = v;
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
    plugins_dir: Option<String>,
    request_timeout_secs: Option<u64>,
    trace_dir: Option<String>,
    session_marker: Option<bool>,
    session_table_depth: Option<usize>,
}
