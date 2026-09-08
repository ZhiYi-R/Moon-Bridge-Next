//! DAO 层：每张表一个模块，方法以 `impl Database` 形式挂载。

pub mod model;
pub mod plugin;
pub mod provider;
pub mod route;
pub mod settings;
pub mod usage;
