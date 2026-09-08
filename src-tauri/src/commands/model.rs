//! Model（模型元数据）与 Offer（provider 报价）CRUD commands。

use std::sync::Arc;

use moonbridge_store::{ModelDef, Offer};
use tauri::State;

use crate::commands::CmdResult;
use crate::state::ManagedState;

/// 列出全部模型定义。
#[tauri::command]
pub fn model_list(state: State<'_, Arc<ManagedState>>) -> CmdResult<Vec<ModelDef>> {
    Ok(state.db.list_models()?)
}

/// 按 slug 获取模型定义。
#[tauri::command]
pub fn model_get(state: State<'_, Arc<ManagedState>>, slug: String) -> CmdResult<Option<ModelDef>> {
    Ok(state.db.get_model(&slug)?)
}

/// 新增或更新模型定义。
#[tauri::command]
pub fn model_save(state: State<'_, Arc<ManagedState>>, model: ModelDef) -> CmdResult<()> {
    Ok(state.db.upsert_model(&model)?)
}

/// 删除模型定义。
#[tauri::command]
pub fn model_delete(state: State<'_, Arc<ManagedState>>, slug: String) -> CmdResult<()> {
    Ok(state.db.delete_model(&slug)?)
}

/// 列出某 provider 提供的模型报价。
#[tauri::command]
pub fn offer_list(state: State<'_, Arc<ManagedState>>, provider_key: String) -> CmdResult<Vec<Offer>> {
    Ok(state.db.list_offers(&provider_key)?)
}

/// 新增或更新报价。
#[tauri::command]
pub fn offer_save(state: State<'_, Arc<ManagedState>>, offer: Offer) -> CmdResult<()> {
    Ok(state.db.upsert_offer(&offer)?)
}

/// 删除报价。
#[tauri::command]
pub fn offer_delete(
    state: State<'_, Arc<ManagedState>>,
    provider_key: String,
    model_slug: String,
) -> CmdResult<()> {
    Ok(state.db.delete_offer(&provider_key, &model_slug)?)
}
