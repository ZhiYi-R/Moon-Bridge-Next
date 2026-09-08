//! Settings（键值设置）commands。业务侧的杂项配置存于 SQLite `settings` 表。

use std::sync::Arc;

use moonbridge_store::Setting;
use serde_json::Value;
use tauri::State;

use crate::commands::CmdResult;
use crate::state::ManagedState;

/// 读取单个设置项。
#[tauri::command]
pub fn settings_get(state: State<'_, Arc<ManagedState>>, key: String) -> CmdResult<Option<Value>> {
    Ok(state.db.get_setting(&key)?)
}

/// 写入单个设置项。
#[tauri::command]
pub fn settings_set(state: State<'_, Arc<ManagedState>>, key: String, value: Value) -> CmdResult<()> {
    Ok(state.db.set_setting(&key, &value)?)
}

/// 列出全部设置项。
#[tauri::command]
pub fn settings_list(state: State<'_, Arc<ManagedState>>) -> CmdResult<Vec<Setting>> {
    Ok(state.db.list_settings()?)
}

/// 删除设置项。
#[tauri::command]
pub fn settings_delete(state: State<'_, Arc<ManagedState>>, key: String) -> CmdResult<()> {
    Ok(state.db.delete_setting(&key)?)
}
