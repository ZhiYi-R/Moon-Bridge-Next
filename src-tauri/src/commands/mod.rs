//! Tauri commands：前端通过 `invoke` 调用的全部后端接口。
//!
//! 分组：网关控制、provider/model/route/plugin/usage/settings CRUD、应用信息。
//! 所有 command 统一用 [`CommandError`]（可序列化）作为错误类型，便于前端展示。

pub mod app;
pub mod gateway;
pub mod model;
pub mod plugin;
pub mod provider;
pub mod route;
pub mod settings;
pub mod trace;
pub mod usage;

use serde::Serialize;

/// 统一的 command 错误类型（前端收到 `{ message }`）。
#[derive(Debug, Serialize)]
pub struct CommandError {
    pub message: String,
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<anyhow::Error> for CommandError {
    fn from(e: anyhow::Error) -> Self {
        CommandError {
            message: format!("{e:#}"),
        }
    }
}

impl From<moonbridge_store::StoreError> for CommandError {
    fn from(e: moonbridge_store::StoreError) -> Self {
        CommandError {
            message: e.to_string(),
        }
    }
}

impl From<String> for CommandError {
    fn from(message: String) -> Self {
        CommandError { message }
    }
}

/// command 统一返回类型。
pub type CmdResult<T> = Result<T, CommandError>;
