//! Moon Bridge Next 基础层。
//!
//! 本 crate 位于分层架构最底部，无内部依赖，定义：
//! - [`ir`]：协议无关的中间表示 Core IR（`CoreRequest` / `CoreResponse` / `CoreStreamEvent` 等）。
//! - [`protocol`]：`Protocol` 枚举，标识入口与上游协议类型。
//! - [`error`]：基础错误类型 `CoreError` 与 `Result` 别名。
//! - [`modelref`]：`model(provider)` 格式的模型引用解析。

pub mod error;
pub mod ir;
pub mod modelref;
pub mod protocol;

pub use error::{CoreError, ErrorBody, ErrorDetail, Result};
pub use ir::{
    ContentBlock, CoreRequest, CoreResponse, CoreStreamEvent, DocSource, Map, Message, Reasoning,
    Role, StopReason, StreamDelta, Tool, ToolChoice, Usage,
};
pub use modelref::ModelRef;
pub use protocol::Protocol;
