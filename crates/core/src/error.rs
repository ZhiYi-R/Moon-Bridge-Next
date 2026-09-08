//! 基础错误类型。库层使用 `thiserror` 定义结构化错误，应用层可用 `anyhow` 包裹。

use serde::{Deserialize, Serialize};

/// Core 层错误。其它 crate 的错误可从中派生或转换为它。
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    /// JSON 序列化/反序列化失败。
    #[error("序列化错误: {0}")]
    Serde(#[from] serde_json::Error),

    /// 协议转换相关错误（附带协议名，便于定位）。
    #[error("协议转换错误[{protocol}]: {message}")]
    Protocol { protocol: String, message: String },

    /// 配置/路由错误。
    #[error("配置错误: {0}")]
    Config(String),

    /// 模型路由无法解析。
    #[error("路由错误: 无法解析模型 {0}")]
    Route(String),

    /// 上游返回错误（HTTP 状态 + 消息）。
    #[error("上游错误[{status}]: {message}")]
    Upstream { status: u16, message: String },

    /// 插件执行错误（附带插件名）。
    #[error("插件错误[{plugin}]: {message}")]
    Plugin { plugin: String, message: String },

    /// 其它未分类错误。
    #[error("{0}")]
    Other(String),
}

impl CoreError {
    /// 构造协议转换错误。
    pub fn protocol(protocol: impl Into<String>, message: impl Into<String>) -> Self {
        CoreError::Protocol {
            protocol: protocol.into(),
            message: message.into(),
        }
    }

    /// 构造插件错误。
    pub fn plugin(plugin: impl Into<String>, message: impl Into<String>) -> Self {
        CoreError::Plugin {
            plugin: plugin.into(),
            message: message.into(),
        }
    }
}

/// 统一 Result 别名。
pub type Result<T> = std::result::Result<T, CoreError>;

/// 面向 HTTP 客户端的错误响应体（OpenAI 风格），便于网关统一输出。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorBody {
    pub error: ErrorDetail,
}

/// 错误详情。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorDetail {
    pub message: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

impl ErrorBody {
    /// 构造一个错误响应体。
    pub fn new(kind: impl Into<String>, message: impl Into<String>) -> Self {
        ErrorBody {
            error: ErrorDetail {
                message: message.into(),
                kind: kind.into(),
                code: None,
            },
        }
    }
}
