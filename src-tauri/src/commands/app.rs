//! 应用级 commands：运行信息、引导配置读写。

use std::sync::Arc;

use serde::Serialize;
use tauri::State;

use crate::commands::CmdResult;
use crate::config::AppConfig;
use crate::state::ManagedState;

/// 应用运行信息（版本 + 关键路径），供「设置 / 关于」页展示。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub version: String,
    pub db_path: String,
    pub config_path: String,
    pub data_dir: String,
    pub plugins_dir: String,
    pub trace_dir: String,
}

/// 获取应用运行信息。
#[tauri::command]
pub fn app_info(state: State<'_, Arc<ManagedState>>) -> CmdResult<AppInfo> {
    let p = &state.paths;
    Ok(AppInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        db_path: p.db_path.to_string_lossy().to_string(),
        config_path: p.config_file.to_string_lossy().to_string(),
        data_dir: p.data_dir.to_string_lossy().to_string(),
        plugins_dir: p.plugins_dir.to_string_lossy().to_string(),
        trace_dir: p.trace_dir.to_string_lossy().to_string(),
    })
}

/// 获取引导配置（网关参数 / 日志级别 / 自启）。
#[tauri::command]
pub fn config_get(state: State<'_, Arc<ManagedState>>) -> CmdResult<AppConfig> {
    Ok(state.config())
}

/// 保存引导配置（落盘 config.toml）。网关参数变更需 `gateway_restart` 生效。
#[tauri::command]
pub fn config_set(state: State<'_, Arc<ManagedState>>, config: AppConfig) -> CmdResult<()> {
    state.update_config(|c| *c = config)?;
    Ok(())
}
