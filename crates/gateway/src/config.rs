//! 网关引导配置（监听地址、认证、egress 代理、配额等）。
//!
//! 与 SQLite 中的业务配置分离：这些是「如何启动网关」的运行时参数，由 app 层
//! 从 `app_config_dir/config.toml` 加载后传入。

use serde::{Deserialize, Serialize};

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
    /// body 大小上限（字节）。三处生效：入站请求体（axum DefaultBodyLimit）、
    /// 报文层钩子处理（超限降级为只读并跳过 Lua）、上游非流式/错误响应体读取
    /// （超限以 502 拒绝）。默认 100MB：大上下文/多图/长文档场景足够，
    /// 又保留了对失控请求的最后防线。
    #[serde(default = "default_max_body")]
    pub max_body_bytes: usize,
    /// 插件脚本目录：`script_ref` 里的相对 `.lua` 路径归一到此处，绝对路径也必须
    /// 落在其中才允许加载（见 `crate::read_script`）。`None` 时退化为按进程 CWD 解析。
    #[serde(default)]
    pub plugins_dir: Option<String>,
    /// 上游请求超时（秒）。
    ///
    /// 两处生效：非流式上游请求的总超时（`dispatch.rs` 发送 + 响应体读取共用
    /// 这一预算；流式刻意不受此限——长生成不应被网关截断），以及插件沙箱的
    /// `call_timeout` 派生基准（见 `lib.rs::sandbox_limits`）。
    /// 上游 HTTP 客户端本身只设连接超时，见 `upstream.rs::build_client`。
    #[serde(default = "default_timeout")]
    pub request_timeout_secs: u64,
    /// trace 文件目录；`None` 表示不写入磁盘。
    #[serde(default)]
    pub trace_dir: Option<String>,
    /// trace 是否记录请求/响应**体**；关闭时只留元数据（方法/URL/头/用量）。
    /// 体可能含用户对话内容与敏感业务数据，按需开关。
    #[serde(default = "default_trace_record_bodies")]
    pub trace_record_bodies: bool,
    /// trace 保留条数：写入磁盘后按 mtime 只保留最近 N 条，`0` 表示不清理。
    #[serde(default = "default_trace_retention")]
    pub trace_retention: usize,
    /// 会话水印开关：把 `[mb:xxxxxx]` 嵌进助手**推理块**明文首部，靠客户端下一轮带回
    /// 来识别会话。入站会先剥除干净再转发上游，上游模型永远看不到它。
    ///
    /// 面向的是**不携带任何会话标识**的客户端（如 Qwen Code：其 `prompt_cache_key`
    /// 注入以直连 `api.openai.com` 为前提）。若你的客户端已通过 `session_id` 字段、
    /// `previous_response_id` 或 `X-Codex-Window-Id` 头表明身份，可关掉本开关。
    /// 代价：标记出现在客户端本地 transcript 的 thinking 明文里（用户可见正文
    /// 不受污染），每轮占约十余 token。关掉后**仍会剥除**入站 marker（客户端
    /// 可能带着开启期间留下的历史），只是不再嵌入。
    #[serde(default = "default_session_marker")]
    pub session_marker: bool,
    /// 活跃会话表的深度上限（LRU，超深淘汰最久未命中者并连带清理其插件态）。
    /// 注意：uuid 载荷自带身份，淘汰不会改变 marker 会话的 session id——表只
    /// 承担外部身份 tag 映射与插件态回收。
    #[serde(default = "default_session_depth")]
    pub session_table_depth: usize,
}

fn default_addr() -> String {
    "127.0.0.1:38440".to_string()
}
fn default_max_body() -> usize {
    100 * 1024 * 1024
}
fn default_timeout() -> u64 {
    300
}
fn default_session_marker() -> bool {
    true
}
fn default_trace_record_bodies() -> bool {
    true
}
fn default_trace_retention() -> usize {
    500
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
            trace_record_bodies: default_trace_record_bodies(),
            trace_retention: default_trace_retention(),
            session_marker: default_session_marker(),
            session_table_depth: default_session_depth(),
        }
    }
}
