//! 基础错误类型。库层使用 `thiserror` 定义结构化错误，应用层可用 `anyhow` 包裹。

use serde::{Deserialize, Serialize};

/// Core 层错误。其它 crate 的错误可从中派生或转换为它。
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("序列化错误: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("协议转换错误[{protocol}]: {message}")]
    Protocol { protocol: String, message: String },

    #[error("配置错误: {0}")]
    Config(String),

    #[error("路由错误: 无法解析模型 {0}")]
    Route(String),

    #[error("上游错误[{status}]: {message}")]
    Upstream { status: u16, message: String },

    #[error("插件错误[{plugin}]: {message}")]
    Plugin { plugin: String, message: String },

    #[error("{0}")]
    Other(String),
}

impl CoreError {
    pub fn protocol(protocol: impl Into<String>, message: impl Into<String>) -> Self {
        CoreError::Protocol {
            protocol: protocol.into(),
            message: message.into(),
        }
    }

    pub fn plugin(plugin: impl Into<String>, message: impl Into<String>) -> Self {
        CoreError::Plugin {
            plugin: plugin.into(),
            message: message.into(),
        }
    }
}

pub type Result<T> = std::result::Result<T, CoreError>;

/// 面向 HTTP 客户端的错误响应体（OpenAI 风格），便于网关统一输出。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorBody {
    pub error: ErrorDetail,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorDetail {
    pub message: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

impl ErrorBody {
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
