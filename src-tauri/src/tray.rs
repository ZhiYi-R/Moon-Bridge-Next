//! 系统托盘：标签页直达、启停网关、退出。

use std::sync::Arc;

use tauri::menu::{IsMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager};

use crate::state::ManagedState;

/// 托盘平铺标签页：（标题，前端路由）。与 `ui/src/router/index.ts`
/// 的路由表保持一致，增删标签页时两边同步改。
const TABS: &[(&str, &str)] = &[
    ("仪表盘", "/dashboard"),
    ("上游服务", "/providers"),
    ("模型", "/models"),
    ("路由", "/routes"),
    ("插件", "/plugins"),
    ("用量", "/usage"),
    ("调用追踪", "/traces"),
    ("设置", "/settings"),
];

/// 构建系统托盘图标与菜单。
pub fn create_tray(app: &AppHandle) -> tauri::Result<()> {
    let mut tab_items = Vec::with_capacity(TABS.len());
    for (title, path) in TABS {
        tab_items.push(MenuItem::with_id(
            app,
            format!("tab:{path}"),
            *title,
            true,
            None::<&str>,
        )?);
    }
    let tab_refs: Vec<&dyn IsMenuItem<tauri::Wry>> = tab_items
        .iter()
        .map(|i| i as &dyn IsMenuItem<tauri::Wry>)
        .collect();
    let toggle = MenuItem::with_id(app, "toggle_gw", "启动 / 停止网关", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;

    let mut top: Vec<&dyn IsMenuItem<tauri::Wry>> = tab_refs;
    top.push(&toggle);
    top.push(&sep);
    top.push(&quit);
    let menu = Menu::with_items(app, &top)?;

    let mut builder = TrayIconBuilder::with_id("main-tray")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .tooltip("Moon Bridge Next — 本地 LLM 网关")
        .on_menu_event(|app, event| match event.id.as_ref() {
            "toggle_gw" => toggle_gateway(app),
            "quit" => quit_app(app),
            id if id.starts_with("tab:") => open_tab(app, &id["tab:".len()..]),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            // 左键单击托盘图标显示主窗口
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        });

    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

/// 显示并聚焦主窗口。
fn show_main_window(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
    }
}

/// 显示主窗口并跳转到指定标签页（前端监听 `navigate-tab` 事件执行路由跳转）。
fn open_tab(app: &AppHandle, path: &str) {
    show_main_window(app);
    let _ = app.emit("navigate-tab", path);
}

/// 优雅退出：先优雅关闭网关（发 shutdown 信号 → serve_with_shutdown 尾部跑
/// 插件 shutdown_all），再退进程。直接 `app.exit` 会跳过插件收尾钩子。
fn quit_app(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let state = app.state::<Arc<ManagedState>>().inner().clone();
        let _ = state.stop_gateway().await;
        app.exit(0);
    });
}

/// 切换网关运行状态（运行中则停止，否则启动）。
fn toggle_gateway(app: &AppHandle) {
    let state = app.state::<Arc<ManagedState>>().inner().clone();
    let running = state.status().running;
    tauri::async_runtime::spawn(async move {
        let result = if running {
            state.stop_gateway().await
        } else {
            state.start_gateway().await
        };
        if let Err(e) = result {
            tracing::error!(error = %e, "托盘切换网关状态失败");
        }
    });
}
