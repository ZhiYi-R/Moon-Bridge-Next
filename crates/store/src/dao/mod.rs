//! DAO 层：每张表一个模块，方法以 `impl Database` 形式挂载。
//!
//! 例外：余额&健康看板两张表（卡片 + 最近结果）成对使用，共用一套行映射与常量，
//! 故合并落在 [`crate::balance`]。

pub mod model;
pub mod plugin;
pub mod provider;
pub mod route;
pub mod secret;
pub mod settings;
pub mod usage;
