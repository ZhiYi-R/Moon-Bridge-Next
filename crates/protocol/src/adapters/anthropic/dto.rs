//! Anthropic Messages API 常量与默认值。

/// 默认 `anthropic-version` 头值。
pub const DEFAULT_VERSION: &str = "2023-06-01";

/// Messages API 路径（拼接到 provider base_url 之后）。
pub const MESSAGES_PATH: &str = "/v1/messages";

/// 当 CoreRequest 未指定 max_tokens 时的兜底值（Anthropic 要求必填）。
pub const DEFAULT_MAX_TOKENS: u32 = 4096;
