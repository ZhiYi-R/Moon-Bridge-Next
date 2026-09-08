//! Provider（上游服务商）CRUD commands。

use std::sync::Arc;

use moonbridge_store::Provider;
use tauri::State;

use crate::commands::CmdResult;
use crate::state::ManagedState;

/// 列出全部 provider。
#[tauri::command]
pub fn provider_list(state: State<'_, Arc<ManagedState>>) -> CmdResult<Vec<Provider>> {
    Ok(state.db.list_providers()?)
}

/// 按 key 获取单个 provider（不存在返回 `None`）。
#[tauri::command]
pub fn provider_get(state: State<'_, Arc<ManagedState>>, key: String) -> CmdResult<Option<Provider>> {
    Ok(state.db.get_provider(&key)?)
}

/// 新增或更新 provider（按 key upsert）。
#[tauri::command]
pub fn provider_save(state: State<'_, Arc<ManagedState>>, provider: Provider) -> CmdResult<()> {
    Ok(state.db.upsert_provider(&provider)?)
}

/// 删除 provider。
#[tauri::command]
pub fn provider_delete(state: State<'_, Arc<ManagedState>>, key: String) -> CmdResult<()> {
    Ok(state.db.delete_provider(&key)?)
}
