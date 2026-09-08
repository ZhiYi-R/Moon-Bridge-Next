//! 插件层错误类型。

/// 插件层错误。
#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    /// mlua 运行时错误（脚本语法、执行、类型转换）。
    #[error("Lua 运行时错误: {0}")]
    Lua(#[from] mlua::Error),

    /// 插件执行期错误（附插件名）。
    #[error("插件[{name}]: {message}")]
    Runtime { name: String, message: String },

    /// 脚本文件 IO 错误。
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),

    /// 清单/配置错误。
    #[error("插件配置错误: {0}")]
    Config(String),

    /// Core 层错误透传。
    #[error(transparent)]
    Core(#[from] moonbridge_core::CoreError),

    /// JSON 序列化错误。
    #[error("序列化错误: {0}")]
    Serde(#[from] serde_json::Error),
}

/// 插件层 Result 别名。
pub type Result<T> = std::result::Result<T, PluginError>;
