//! Plugin（Lua 插件）与 PluginBinding（作用域绑定）CRUD commands。
//!
//! 注意：插件在网关 `bootstrap` 时加载为 `PluginHooks`，因此增删改插件后需
//! 调用 `gateway_restart` 方能生效（前端在保存后触发）。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use moonbridge_store::{PluginBinding, PluginRecord};
use tauri::State;

use crate::commands::CmdResult;
use crate::state::ManagedState;

/// 解析插件脚本的文件路径：`script_ref` 以 `.lua` 结尾视为文件脚本，
/// 相对路径优先归一到 `plugins_dir`；否则（非 `.lua`）视为内联脚本，返回 `None`。
fn resolve_script_file(state: &ManagedState, script_ref: &str) -> Option<PathBuf> {
    let trimmed = script_ref.trim();
    if !trimmed.ends_with(".lua") {
        return None;
    }
    let p = Path::new(trimmed);
    if p.is_absolute() {
        return Some(p.to_path_buf());
    }
    let in_plugins = state.paths.plugins_dir.join(p);
    if in_plugins.exists() {
        return Some(in_plugins);
    }
    if p.exists() {
        return Some(p.to_path_buf());
    }
    // 尚不存在：归一到 plugins_dir 下
    Some(in_plugins)
}

/// 列出全部插件。
#[tauri::command]
pub fn plugin_list(state: State<'_, Arc<ManagedState>>) -> CmdResult<Vec<PluginRecord>> {
    Ok(state.db.list_plugins()?)
}

/// 按名获取插件。
#[tauri::command]
pub fn plugin_get(state: State<'_, Arc<ManagedState>>, name: String) -> CmdResult<Option<PluginRecord>> {
    Ok(state.db.get_plugin(&name)?)
}

/// 新增或更新插件记录。
#[tauri::command]
pub fn plugin_save(state: State<'_, Arc<ManagedState>>, plugin: PluginRecord) -> CmdResult<()> {
    Ok(state.db.upsert_plugin(&plugin)?)
}

/// 删除插件。
#[tauri::command]
pub fn plugin_delete(state: State<'_, Arc<ManagedState>>, name: String) -> CmdResult<()> {
    Ok(state.db.delete_plugin(&name)?)
}

/// 读取插件脚本内容：`.lua` 文件脚本读文件（相对路径归一到 `plugins_dir`），
/// 否则视为内联脚本直接返回 `script_ref`。
#[tauri::command]
pub fn plugin_read_script(state: State<'_, Arc<ManagedState>>, name: String) -> CmdResult<String> {
    let rec = state
        .db
        .get_plugin(&name)?
        .ok_or_else(|| format!("插件不存在: {name}"))?;
    if let Some(file) = resolve_script_file(&state, &rec.script_ref) {
        if file.exists() {
            return Ok(std::fs::read_to_string(&file).map_err(|e| e.to_string())?);
        }
        return Ok(String::new()); // 文件脚本但尚未落盘
    }
    // 兼容：script_ref 为其它既存路径则读文件，否则作内联脚本
    let p = Path::new(&rec.script_ref);
    if p.exists() {
        Ok(std::fs::read_to_string(p).unwrap_or_else(|_| rec.script_ref.clone()))
    } else {
        Ok(rec.script_ref)
    }
}

/// 写入（保存）插件脚本内容，供前端在线编辑。
///
/// 文件脚本（`.lua`）：写入解析出的路径（必要时创建父目录），并把 `script_ref`
/// 规范化为绝对路径，确保网关按原样读取时能定位；内联脚本：直接写回记录。
#[tauri::command]
pub fn plugin_write_script(
    state: State<'_, Arc<ManagedState>>,
    name: String,
    content: String,
) -> CmdResult<()> {
    let mut rec = state
        .db
        .get_plugin(&name)?
        .ok_or_else(|| format!("插件不存在: {name}"))?;
    if let Some(file) = resolve_script_file(&state, &rec.script_ref) {
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&file, content).map_err(|e| e.to_string())?;
        let abs = file.to_string_lossy().to_string();
        if rec.script_ref != abs {
            rec.script_ref = abs;
            state.db.upsert_plugin(&rec)?;
        }
    } else {
        rec.script_ref = content;
        state.db.upsert_plugin(&rec)?;
    }
    Ok(())
}

/// 列出某插件的全部作用域绑定。
#[tauri::command]
pub fn binding_list(state: State<'_, Arc<ManagedState>>, plugin_name: String) -> CmdResult<Vec<PluginBinding>> {
    Ok(state.db.list_bindings(&plugin_name)?)
}

/// 新增或更新作用域绑定。
#[tauri::command]
pub fn binding_save(state: State<'_, Arc<ManagedState>>, binding: PluginBinding) -> CmdResult<()> {
    Ok(state.db.upsert_binding(&binding)?)
}

/// 列出某一作用域类型的全部绑定（scope：provider/model/route/global）。
#[tauri::command]
pub fn binding_list_by_scope(
    state: State<'_, Arc<ManagedState>>,
    scope: String,
) -> CmdResult<Vec<PluginBinding>> {
    Ok(state.db.list_bindings_by_scope(&scope)?)
}

/// 删除作用域绑定（三态开关重置回「跟随全局」时调用）。
#[tauri::command]
pub fn binding_delete(
    state: State<'_, Arc<ManagedState>>,
    plugin_name: String,
    scope: String,
    scope_key: String,
) -> CmdResult<()> {
    Ok(state.db.delete_binding(&plugin_name, &scope, &scope_key)?)
}
