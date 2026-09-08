//! 网关生命周期控制 commands：启动 / 停止 / 重启 / 状态查询。

use std::sync::Arc;

use tauri::State;

use crate::commands::{CmdResult, CommandError};
use crate::state::{GatewayStatus, ManagedState};

/// 启动内嵌网关。幂等：已运行则直接返回当前状态。
#[tauri::command]
pub async fn gateway_start(state: State<'_, Arc<ManagedState>>) -> CmdResult<GatewayStatus> {
    state.start_gateway().await.map_err(CommandError::from)
}

/// 停止内嵌网关（优雅关闭）。
#[tauri::command]
pub async fn gateway_stop(state: State<'_, Arc<ManagedState>>) -> CmdResult<GatewayStatus> {
    state.stop_gateway().await.map_err(CommandError::from)
}

/// 重启网关（用于插件 / provider 配置变更后热加载）。
#[tauri::command]
pub async fn gateway_restart(state: State<'_, Arc<ManagedState>>) -> CmdResult<GatewayStatus> {
    state.stop_gateway().await.map_err(CommandError::from)?;
    state.start_gateway().await.map_err(CommandError::from)
}

/// 查询网关运行状态。
#[tauri::command]
pub fn gateway_status(state: State<'_, Arc<ManagedState>>) -> CmdResult<GatewayStatus> {
    Ok(state.status())
}
