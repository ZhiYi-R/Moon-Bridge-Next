//! DAO 层：每张表一个模块，方法以 `impl Database` 形式挂载。
//!
//! 例外：配额查询结果按 Provider+端点维度成对使用，相关读写与到期判定
//! 集中在 [`crate::quota`]。

pub mod model;
pub mod plugin;
pub mod provider;
pub mod route;
pub mod settings;
pub mod usage;
