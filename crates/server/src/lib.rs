//! Moon Bridge Next 服务端（web 模式）。
//!
//! 纯 axum 二进制，不依赖 Tauri/WebKit：在同一监听端口上同时提供
//! - LLM 网关路由（复用 [`moonbridge_gateway::server::router`]，含 `/health`）；
//! - `/api/*` 管理 REST API（Bearer 认证，语义 1:1 对齐桌面端 Tauri commands）；
//! - 前端静态文件托管（SPA，未命中时回落到 `index.html`）。
//!
//! 模块划分：
//! - [`args`]：命令行参数解析（含 `--help`）。
//! - [`config`]：引导配置与关键路径（与桌面端共享 `config.toml` 格式）。
//! - [`admin`]：管理 API 路由与 handlers。
//! - [`serve`]：合并路由与进程生命周期。

pub mod admin;
pub mod args;
pub mod config;
pub mod lifecycle;
pub mod serve;
