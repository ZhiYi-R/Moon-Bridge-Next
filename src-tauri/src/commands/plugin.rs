//! Plugin（Lua 插件）与 PluginBinding（作用域绑定）CRUD commands。
//!
//! 注意：插件在网关 `bootstrap` 时加载为 `PluginHooks`，因此增删改插件后需
//! 调用 `gateway_restart` 方能生效（前端在保存后触发）。

use std::path::PathBuf;
use std::sync::Arc;

use moonbridge_gateway::{parse_script_ref, ScriptRef};
use moonbridge_store::{PluginBinding, PluginRecord};
use tauri::State;

use crate::commands::CmdResult;
use crate::state::ManagedState;

/// 解析脚本引用为**受约束**的文件路径；`Ok(None)` 表示内联脚本。
///
/// 判定与网关加载侧共用 [`parse_script_ref`]（以 `.lua` 结尾 = 文件脚本），并强制
/// 相对路径归一到 `plugins_dir`、绝对路径必须落在 `plugins_dir` 之内。
///
/// 历史缺陷：旧实现把绝对 `script_ref` 原样放行，`plugin_read_script` 还有一条
/// 「只要 `Path::exists()` 就读」的兜底——等于任意文件读取原语；配合
/// `plugin_write_script` 即任意文件写入。同目录 `trace.rs` 早有 `resolve_within`
/// 做包含性校验，此处缺失属遗漏。
fn script_file(state: &ManagedState, script_ref: &str) -> CmdResult<Option<PathBuf>> {
    match parse_script_ref(script_ref, Some(state.paths.plugins_dir.as_path())) {
        ScriptRef::File(p) => Ok(Some(p)),
        ScriptRef::Inline(_) => Ok(None),
        ScriptRef::Rejected(p) => Err(format!("插件脚本路径越出插件目录: {p}").into()),
    }
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
    match script_file(&state, &rec.script_ref)? {
        Some(file) => {
            if file.is_file() {
                Ok(std::fs::read_to_string(&file).map_err(|e| e.to_string())?)
            } else {
                Ok(String::new()) // 文件脚本但尚未落盘
            }
        }
        None => Ok(rec.script_ref), // 内联脚本
    }
}

/// 写入（保存）插件脚本内容，供前端在线编辑。
///
/// 文件脚本（`.lua`）：写入**已收敛到 `plugins_dir` 内**的路径（必要时创建父目录），
/// 并把 `script_ref` 规范化为该绝对路径，确保网关按同一路径读取；内联脚本：直接写回
/// 记录。越出 `plugins_dir` 的引用由 [`script_file`] 判为错误，写不到目录外。
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
    match script_file(&state, &rec.script_ref)? {
        Some(file) => {
            if let Some(parent) = file.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            std::fs::write(&file, content).map_err(|e| e.to_string())?;
            let abs = file.to_string_lossy().to_string();
            if rec.script_ref != abs {
                rec.script_ref = abs;
                state.db.upsert_plugin(&rec)?;
            }
        }
        None => {
            rec.script_ref = content;
            state.db.upsert_plugin(&rec)?;
        }
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
