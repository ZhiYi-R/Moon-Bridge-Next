//! Moon Bridge Next 桌面应用（Tauri 壳）。
//!
//! 职责：装配 Tauri 插件、初始化日志与 [`ManagedState`]（打开数据库、加载引导
//! 配置）、构建系统托盘、按需自启动内嵌网关，并注册全部前端 commands。
//!
//! 依赖方向：app → gateway, store, core（不反向）。

mod commands;
mod config;
mod state;
mod tray;

use config::AppPaths;
use state::ManagedState;
use tauri::Manager;
use tracing_subscriber::EnvFilter;

/// 应用入口。
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Rust 侧结构化日志用 tracing；`log` crate 的全局 logger 让给 tauri-plugin-log
    // 承接（前端 console 与依赖库的 log 记录），二者分属不同后端、互不冲突。
    //
    // 必须用 `set_global_default` 而非 `tracing_subscriber::fmt().try_init()`：后者在
    // 默认启用 `tracing-log` 特性时会调用 `LogTracer::init()` 抢占全局 `log` logger，
    // 导致随后 tauri-plugin-log 初始化失败并 panic
    // （"attempted to set a logger after the logging system was already initialized"）。
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .finish();
    let _ = tracing::subscriber::set_global_default(subscriber);

    let mut builder = tauri::Builder::default();

    // 单实例（桌面）：第二个实例启动时聚焦已有窗口。须最先注册。
    #[cfg(desktop)]
    {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(win) = app.get_webview_window("main") {
                let _ = win.show();
                let _ = win.unminimize();
                let _ = win.set_focus();
            }
        }));
    }

    builder
        .plugin(tauri_plugin_log::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        // 窗口状态恢复不包含 decorations：否则会把首次运行时保存的“有系统装饰”状态
        // 恢复回来，覆盖 tauri.conf.json 的 decorations:false（自绘标题栏必需）。
        .plugin(
            tauri_plugin_window_state::Builder::new()
                .with_state_flags(
                    tauri_plugin_window_state::StateFlags::all()
                        & !tauri_plugin_window_state::StateFlags::DECORATIONS,
                )
                .build(),
        )
        .setup(|app| {
            // 解析应用目录 → 打开数据库 + 加载引导配置 → 托管状态
            let config_dir = app.path().app_config_dir()?;
            let data_dir = app.path().app_data_dir()?;
            let paths = AppPaths::resolve(config_dir, data_dir);
            let state = ManagedState::new(paths)?;

            let auto_start = state.config().auto_start;
            app.manage(state.clone());

            // 系统托盘
            tray::create_tray(app.handle())?;

            // 按引导配置自启动网关
            if auto_start {
                let s = state.clone();
                tauri::async_runtime::spawn(async move {
                    match s.start_gateway().await {
                        Ok(st) => tracing::info!(running = st.running, addr = %st.addr, "网关自启动完成"),
                        Err(e) => tracing::error!(error = %e, "网关自启动失败"),
                    }
                });
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // 网关生命周期
            commands::gateway::gateway_start,
            commands::gateway::gateway_stop,
            commands::gateway::gateway_restart,
            commands::gateway::gateway_status,
            // Provider
            commands::provider::provider_list,
            commands::provider::provider_get,
            commands::provider::provider_save,
            commands::provider::provider_delete,
            // Model & Offer
            commands::model::model_list,
            commands::model::model_get,
            commands::model::model_save,
            commands::model::model_delete,
            commands::model::offer_list,
            commands::model::offer_save,
            commands::model::offer_delete,
            // 模型目录（models.dev 拉取 + 导入）
            commands::catalog::catalog_fetch,
            commands::catalog::catalog_import,
            // Route
            commands::route::route_list,
            commands::route::route_get,
            commands::route::route_save,
            commands::route::route_delete,
            // Plugin & Binding
            commands::plugin::plugin_list,
            commands::plugin::plugin_get,
            commands::plugin::plugin_save,
            commands::plugin::plugin_delete,
            commands::plugin::plugin_read_script,
            commands::plugin::plugin_write_script,
            commands::plugin::binding_list,
            commands::plugin::binding_save,
            commands::plugin::binding_list_by_scope,
            commands::plugin::binding_delete,
            // Usage
            commands::usage::usage_query,
            commands::usage::usage_summary,
            // Trace
            commands::trace::trace_list,
            commands::trace::trace_read,
            commands::trace::trace_delete,
            // Settings
            commands::settings::settings_get,
            commands::settings::settings_set,
            commands::settings::settings_list,
            commands::settings::settings_delete,
            // App
            commands::app::app_info,
            commands::app::config_get,
            commands::app::config_set,
        ])
        .run(tauri::generate_context!())
        .expect("运行 Moon Bridge Next 时发生错误");
}
