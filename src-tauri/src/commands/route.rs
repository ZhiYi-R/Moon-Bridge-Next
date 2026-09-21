//! Route（模型别名路由）CRUD commands。

use std::sync::Arc;

use moonbridge_store::Route;
use tauri::State;

use crate::commands::CmdResult;
use crate::state::ManagedState;

#[tauri::command]
pub fn route_list(state: State<'_, Arc<ManagedState>>) -> CmdResult<Vec<Route>> {
    Ok(state.db.list_routes()?)
}

#[tauri::command]
pub fn route_get(state: State<'_, Arc<ManagedState>>, alias: String) -> CmdResult<Option<Route>> {
    Ok(state.db.get_route(&alias)?)
}

#[tauri::command]
pub fn route_save(state: State<'_, Arc<ManagedState>>, route: Route) -> CmdResult<()> {
    Ok(state.db.upsert_route(&route)?)
}

#[tauri::command]
pub fn route_delete(state: State<'_, Arc<ManagedState>>, alias: String) -> CmdResult<()> {
    Ok(state.db.delete_route(&alias)?)
}
