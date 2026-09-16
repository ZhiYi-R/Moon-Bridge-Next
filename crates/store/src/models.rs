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

fn default_display_mode() -> String {
    "auto".to_string()
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
    /// 输出 token 上限（models.dev `limit.output`）。Anthropic 等要求必填
    /// `max_tokens` 的上游协议在客户端未设上限时以此兜底，不凭空注入小值。
    #[serde(default)]
    pub max_output_tokens: Option<i64>,
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

/// 余额&健康看板卡片。
///
/// 每张卡片绑一段 Lua 脚本（`script_ref` 判定与插件一致：`.lua` 结尾 = `plugins_dir`
/// 内文件，否则内联源码），脚本暴露 `MB.query(ctx)` 返回配额/余额信息。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BalanceCard {
    /// 唯一 key（卡片名，亦作为脚本 ctx 的 `name`）。
    pub key: String,
    /// 引用的上游 Provider key（`providers.key`）。设置后脚本 ctx 的 `key`/`keys`
    /// 由该 Provider 的端点解析（key 去重）；`None` 时为旧版手填模式，回落到 `api_key`。
    #[serde(default)]
    pub provider_key: Option<String>,
    /// 显示样式：`auto`（按数据自动）| `percent`（强制百分比）| `amount`（强制金额）。
    /// 纯前端展示 concern，引擎不参与；保存时非法值归一为 `auto`。
    #[serde(default = "default_display_mode")]
    pub display_mode: String,
    /// 卡片 API Key（明文）。旧版手填模式专用，设置 `provider_key` 后忽略。
    #[serde(default)]
    pub api_key: String,
    /// 查询 URL（脚本 ctx 的 `base_url`）。用户可选填写的配额接口基准地址，
    /// 与 Provider 端点无关、两种模式通用；留空则脚本需自行处理 URL。
    #[serde(default)]
    pub base_url: String,
    /// 服务商展示标签（脚本 ctx 的 `provider`；引用模式下留空则取 Provider key）。
    #[serde(default)]
    pub provider_label: String,
    /// 脚本引用：`.lua` 结尾为 `plugins_dir` 内文件，否则内联 Lua 源码。
    #[serde(default)]
    pub script_ref: String,
    /// 定时查询间隔（秒）；`0` 表示禁用定时、只手动刷新。保存时 `1..=59` 夹到 60。
    #[serde(default)]
    pub interval_secs: i64,
    /// 是否启用（禁用后不参与 `balance_refresh_all` 与定时调度）。
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 卡片自定义参数（原样透传脚本 ctx 的 `extra`）。
    #[serde(default)]
    pub extra: Value,
    /// 展示排序（升序）。
    #[serde(default)]
    pub position: i64,
    /// 创建时间（unix 秒）。
    #[serde(default)]
    pub created_at: i64,
    /// 更新时间（unix 秒）。
    #[serde(default)]
    pub updated_at: i64,
}

/// 余额卡片最近一次查询结果（一卡一行，随卡片删除级联清理）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BalanceResult {
    /// `ok` | `error`。
    pub status: String,
    /// 脚本返回的完整 JSON（`quotas` 已归一为 camelCase 并补齐 used/left 互补值，
    /// 其余字段原样保留）；引擎级失败时保留上一次的值。
    #[serde(default)]
    pub payload: Option<Value>,
    /// 失败原因（引擎级或脚本业务 `message`）。
    #[serde(default)]
    pub error: Option<String>,
    /// 查询时间（unix 秒）。
    #[serde(default)]
    pub queried_at: i64,
}

/// 反序列化「字符串或数字」为 `Option<String>`：数字（unix 秒时间戳）转成十进制字符串，
/// 其余类型（bool/数组/对象/null）视为 None——脚本给错形状时不因一个字段丢掉整条配额。
fn de_opt_string_or_number<'de, D>(d: D) -> std::result::Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Option::<Value>::deserialize(d)? {
        Some(Value::String(s)) => Ok(Some(s)),
        Some(Value::Number(n)) => Ok(Some(n.to_string())),
        _ => Ok(None),
    }
}

/// 余额脚本返回的单条配额（`payload.quotas` 的元素）。
///
/// 脚本按 Lua 习惯写 snake_case，引擎落库前归一为 camelCase（本结构的序列化形状）；
/// `used_percent` / `left_percent` 互补，脚本只给一个时由引擎补另一个。
/// 金额模式：脚本给出 `unit` + `used_amount`（可再带 `left_amount`）时，前端按
/// 「消耗 x{unit} · 余额 y{unit}」展示；amount 字段之间以及与 percent 之间均不做互补互推。
/// 脚本自定义的额外字段经 `extra` 原样保留。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BalanceQuota {
    /// 配额展示名（如「5 小时窗口」）。
    pub label: String,
    /// 已用百分比。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        alias = "used_percent"
    )]
    pub used_percent: Option<f64>,
    /// 剩余百分比。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        alias = "left_percent"
    )]
    pub left_percent: Option<f64>,
    /// 金额单位（如 `¥`、`$`、`GB`），仅金额模式使用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// 已用金额（金额模式）。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        alias = "used_amount"
    )]
    pub used_amount: Option<f64>,
    /// 剩余金额（金额模式）。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        alias = "left_amount"
    )]
    pub left_amount: Option<f64>,
    /// 重置时间：脚本可给字符串（原样展示）或 unix 秒数字（归一为字符串，前端识别纯数字
    /// 后按时间戳格式化——沙箱无 os 库，脚本无法自行格式化毫秒时间戳，故契约放行数字）。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        alias = "reset_at",
        deserialize_with = "de_opt_string_or_number"
    )]
    pub reset_at: Option<String>,
    /// 脚本自定义字段。
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

/// 余额卡片视图：卡片配置 + 逐 key 的最近一次查询结果（空数组表示从未查询过）。
///
/// 多 key 卡片按 key 拆卡展示：引擎对解析出的每个 key 各跑一次脚本，每个 key 一行
/// 结果。REST 与 IPC 两个宿主共用同一形状，前端一次拿到卡片与其全部 key 的余额。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BalanceCardView {
    #[serde(flatten)]
    pub card: BalanceCard,
    #[serde(default)]
    pub results: Vec<BalanceKeyResult>,
}

/// 单个 key 的查询结果（`balance_results` 一行的视图，按 `(card_key, key_index)` 定位）。
///
/// `key_label` 是掩码后的 key 展示标签（如 `sk-kim…LXyw`），由引擎在运行时写入；
/// 前端展示 key 身份只认它，绝不接触 key 原文。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BalanceKeyResult {
    /// key 在本次解析结果中的序号（0 起，与 `ctx.keys` 的顺序口径一致）。
    #[serde(default)]
    pub key_index: i64,
    /// 掩码后的 key 展示标签；无 key（手填模式空 key 或解析失败）时为空串。
    #[serde(default)]
    pub key_label: String,
    /// 本次查询结果。
    #[serde(flatten)]
    pub result: BalanceResult,
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
    /// 命中的 provider key（定价按 (provider, model) 检索的依据）；路由前失败为 None。
    #[serde(default)]
    pub provider_key: Option<String>,
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
    /// 请求状态：ok / error / aborted（流未读尽即结束，如客户端断开）。
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
    /// 按 provider key 过滤（成本维度：定价是 (provider, model) 粒度）。
    pub provider_key: Option<String>,
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
