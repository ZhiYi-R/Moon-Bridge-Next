//! Provider（上游服务商）CRUD commands。

use std::sync::Arc;

use moonbridge_store::Provider;
use tauri::State;

use crate::commands::CmdResult;
use crate::presets::ProviderPreset;
use crate::state::ManagedState;

/// 列出全部上游预设（静态表；账户组走 OAuth 登录编排）。
#[tauri::command]
pub fn preset_list() -> CmdResult<Vec<ProviderPreset>> {
    Ok(crate::presets::PRESETS.to_vec())
}

/// 列出全部 provider。
#[tauri::command]
pub fn provider_list(state: State<'_, Arc<ManagedState>>) -> CmdResult<Vec<Provider>> {
    Ok(state.db.list_providers()?)
}

/// 按 key 获取单个 provider（不存在返回 `None`）。
#[tauri::command]
pub fn provider_get(
    state: State<'_, Arc<ManagedState>>,
    key: String,
) -> CmdResult<Option<Provider>> {
    Ok(state.db.get_provider(&key)?)
}

#[tauri::command]
pub fn provider_save(state: State<'_, Arc<ManagedState>>, provider: Provider) -> CmdResult<()> {
    Ok(state.db.upsert_provider(&provider)?)
}

#[tauri::command]
pub fn provider_delete(state: State<'_, Arc<ManagedState>>, key: String) -> CmdResult<()> {
    Ok(state.db.delete_provider(&key)?)
}
