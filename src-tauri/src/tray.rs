//! 系统托盘：显示主窗口、一键启停网关、退出。

use std::sync::Arc;

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};

use crate::state::ManagedState;

/// 构建系统托盘图标与菜单。
pub fn create_tray(app: &AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "显示主窗口", true, None::<&str>)?;
    let toggle = MenuItem::with_id(app, "toggle_gw", "启动 / 停止网关", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[&show, &toggle, &PredefinedMenuItem::separator(app)?, &quit],
    )?;

    let mut builder = TrayIconBuilder::with_id("main-tray")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .tooltip("Moon Bridge Next — 本地 LLM 网关")
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => show_main_window(app),
            "toggle_gw" => toggle_gateway(app),
            "quit" => app.exit(0),
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
