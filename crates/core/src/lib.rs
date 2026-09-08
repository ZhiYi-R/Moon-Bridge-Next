//! Moon Bridge Next 基础层。
//!
//! 本 crate 位于分层架构最底部，不依赖任何其它内部 crate。它定义了：
//! - [`ir`]：协议中立的 Core IR（`CoreRequest` / `CoreResponse` / `CoreStreamEvent` 等），
//!   所有入口协议与上游协议的 Adapter 都以 Core IR 为中间表示进行转换，从而把
//!   N 个入口 × M 个上游的转换矩阵降为 N + M。
//! - [`protocol`]：`Protocol` 枚举，标识入口/上游协议种类。
//! - [`error`]：基础错误类型 `CoreError` 与 `Result` 别名。
//! - [`modelref`]：`model(provider)` 格式模型引用的解析与规范化。
//!
//! 设计原则：Clean room，仅依赖 serde / serde_json，不引入任何协议特定实现。

pub mod error;
pub mod ir;
pub mod modelref;
pub mod protocol;

pub use error::{CoreError, ErrorBody, ErrorDetail, Result};
pub use ir::{
    ContentBlock, CoreRequest, CoreResponse, CoreStreamEvent, Map, Message, Reasoning, Role,
    StopReason, StreamDelta, Tool, ToolChoice, Usage,
};
pub use modelref::ModelRef;
pub use protocol::Protocol;
