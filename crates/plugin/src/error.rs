#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("Lua 运行时错误: {0}")]
    Lua(#[from] mlua::Error),

    #[error("插件[{name}]: {message}")]
    Runtime { name: String, message: String },

    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),

    #[error("插件配置错误: {0}")]
    Config(String),

    #[error(transparent)]
    Core(#[from] moonbridge_core::CoreError),

    #[error("序列化错误: {0}")]
    Serde(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, PluginError>;
