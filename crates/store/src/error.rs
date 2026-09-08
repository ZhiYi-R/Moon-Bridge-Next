//! 存储层错误类型。

/// 存储层错误。
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// 底层 SQLite 错误。
    #[error("数据库错误: {0}")]
    Db(#[from] rusqlite::Error),

    /// JSON 序列化错误（配置/extra 字段）。
    #[error("序列化错误: {0}")]
    Serde(#[from] serde_json::Error),

    /// 记录不存在。
    #[error("未找到: {0}")]
    NotFound(String),

    /// 其它错误。
    #[error("{0}")]
    Other(String),
}

/// 存储层 Result 别名。
pub type Result<T> = std::result::Result<T, StoreError>;
