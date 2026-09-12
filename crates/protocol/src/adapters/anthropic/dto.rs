//! Anthropic Messages API 常量与默认值。

/// 默认 `anthropic-version` 头值。
pub const DEFAULT_VERSION: &str = "2023-06-01";

/// Messages API 路径（拼接到 provider base_url 之后）。
pub const MESSAGES_PATH: &str = "/v1/messages";

/// `max_tokens` 的最后兜底（Anthropic 要求必填）：仅在客户端未设上限且
/// models 表也无 `max_output_tokens` 元数据时才用到。
pub const DEFAULT_MAX_TOKENS: u32 = 4096;
