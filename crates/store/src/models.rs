//! 存储层数据模型（DTO）。这些类型是 DAO 与上层（gateway/app）交换的数据结构，
//! 与 SQLite 表一一对应。JSON 类字段以 `serde_json::Value` 承载，保持灵活。

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 上游 Provider 的单个端点：协议 + Base URL + 独立 API Key。
/// 一个 Provider 可配置多个端点实现故障转移；Key 留空则回退使用前一个非空 Key。
/// 协议绑定在端点上：同一 Provider 可混合不同协议的端点。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Endpoint {
    /// 上游协议标识（对应 `moonbridge_core::Protocol::as_str`）。
    pub protocol: String,
    /// 基础 URL。
    pub base_url: String,
    /// API Key（明文；落库时由 `EncKey` 加密）。
    #[serde(default)]
    pub api_key: String,
}

/// 上游 Provider 配置。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Provider {
    /// 唯一 key。
    pub key: String,
    /// 端点列表（按序故障转移；至少一个；协议绑定在端点上）。
    #[serde(default)]
    pub endpoints: Vec<Endpoint>,
    /// 协议版本头（如 Anthropic `2023-06-01`）。
    #[serde(default)]
    pub version: Option<String>,
    /// 自定义 User-Agent。
    #[serde(default)]
    pub user_agent: Option<String>,
    /// Web Search 配置（原始 JSON）。
    #[serde(default)]
    pub web_search: Option<Value>,
    /// 协议特定额外字段。
    #[serde(default)]
    pub extra: Value,
    /// 是否启用。
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 创建时间（unix 秒）。
    #[serde(default)]
    pub created_at: i64,
    /// 更新时间（unix 秒）。
    #[serde(default)]
    pub updated_at: i64,
}

fn default_true() -> bool {
    true
}

/// 模型元数据定义（仅承载模型自身属性；定价口径统一在 offers）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelDef {
    pub slug: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub context_window: Option<i64>,
    #[serde(default)]
    pub modalities: Option<Value>,
    #[serde(default)]
    pub reasoning_levels: Option<Value>,
    #[serde(default)]
    pub extra: Value,
}

/// Provider 提供的模型报价。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Offer {
    pub provider_key: String,
    pub model_slug: String,
    #[serde(default)]
    pub pricing: Option<Value>,
    /// 绑定到 provider 的特定协议端点（如 `openai-response`）。非空时路由命中该 offer
    /// 后只用匹配该协议的端点故障转移；为空则用该 provider 的全部端点（旧行为）。
    #[serde(default)]
    pub endpoint_protocol: Option<String>,
}

/// 路由别名：把客户端请求的别名映射到 (provider, model)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Route {
    pub alias: String,
    pub model_slug: String,
    pub provider_key: String,
    #[serde(default)]
    pub extra: Value,
}

/// 插件记录。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginRecord {
    pub name: String,
    /// 插件来源类型，目前固定 `lua`。
    #[serde(default = "default_source")]
    pub source: String,
    /// 脚本引用：文件路径或内联脚本标识。
    pub script_ref: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 插件全局配置（原始 JSON）。
    #[serde(default)]
    pub config: Value,
    /// 作用域列表：global/provider/model/route。
    #[serde(default)]
    pub scopes: Vec<String>,
    /// 能力声明：core/raw_request/raw_response/raw_stream。
    #[serde(default)]
    pub capabilities: Vec<String>,
}

fn default_source() -> String {
    "lua".to_string()
}

/// 插件在具体作用域上的启用/配置覆盖。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginBinding {
    pub plugin_name: String,
    /// global / provider / model / route。
    pub scope: String,
    /// 作用域键（provider key、model slug、route alias）；global 时为空串。
    #[serde(default)]
    pub scope_key: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub config: Value,
}

/// 用量记录。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRecord {
    pub id: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub upstream_model: Option<String>,
    #[serde(default)]
    pub input_tokens: u32,
    #[serde(default)]
    pub output_tokens: u32,
    #[serde(default)]
    pub cache_read_tokens: u32,
    #[serde(default)]
    pub cache_write_tokens: u32,
    #[serde(default)]
    pub reasoning_tokens: u32,
    #[serde(default)]
    pub cost: f64,
    /// 请求状态：ok / error。
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub latency_ms: i64,
    /// 首字延迟（TTFT，毫秒）；仅流式请求有值，非流式/错误为 None。
    #[serde(default)]
    pub ttft_ms: Option<i64>,
    #[serde(default)]
    pub created_at: i64,
}

/// 用量查询过滤条件。
#[derive(Debug, Clone, Default)]
pub struct UsageQuery {
    pub model: Option<String>,
    pub status: Option<String>,
    pub since: Option<i64>,
    pub until: Option<i64>,
    pub limit: i64,
    pub offset: i64,
}

/// 键值设置项。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Setting {
    pub key: String,
    pub value: Value,
}
