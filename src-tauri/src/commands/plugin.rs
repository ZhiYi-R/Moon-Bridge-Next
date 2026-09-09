//! Plugin（Lua 插件）与 PluginBinding（作用域绑定）CRUD commands。
//!
//! 注意：插件在网关 `bootstrap` 时加载为 `PluginHooks`，因此增删改插件后需
//! 调用 `gateway_restart` 方能生效（前端在保存后触发）。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use moonbridge_gateway::{parse_script_ref, ScriptRef};
use moonbridge_store::{PluginBinding, PluginRecord};
use serde::Serialize;
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
    write_script_inner(state.inner(), &name, &content)
}

/// [`plugin_write_script`] 的内部实现，供导入流程复用。
fn write_script_inner(state: &ManagedState, name: &str, content: &str) -> CmdResult<()> {
    let mut rec = state
        .db
        .get_plugin(name)?
        .ok_or_else(|| format!("插件不存在: {name}"))?;
    match script_file(state, &rec.script_ref)? {
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
            rec.script_ref = content.to_string();
            state.db.upsert_plugin(&rec)?;
        }
    }
    Ok(())
}

/// 从 Lua 脚本中尽力提取 `key = { "a", "b" }` 形式的字符串列表（导入时读取脚本
/// 自带的 scopes/capabilities 声明）；找不到回退空表。忽略转义与注释，尽力而为。
fn extract_lua_string_list(script: &str, key: &str) -> Vec<String> {
    let Some(pos) = script.find(key) else {
        return Vec::new();
    };
    let rest = &script[pos + key.len()..];
    let Some(open) = rest.find('{') else {
        return Vec::new();
    };
    let Some(close) = rest[open..].find('}') else {
        return Vec::new();
    };
    rest[open + 1..open + close]
        .split('"')
        .enumerate()
        .filter(|(i, _)| i % 2 == 1)
        .map(|(_, s)| s.to_string())
        .collect()
}

/// 单个文件的导入结果。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginImportOutcome {
    pub path: String,
    pub name: String,
    /// imported / skipped / error
    pub status: String,
    pub message: Option<String>,
}

/// 从磁盘导入 `.lua` 插件文件：以文件名（去扩展名）为插件名，脚本拷贝进
/// `plugins_dir` 并落库；同名插件已存在时跳过（不覆盖）。scopes/capabilities
/// 尽力从脚本 `MB = { scopes = {...}, capabilities = {...} }` 声明提取，缺省
/// global/core。逐文件返回结果，单个失败不影响其余。
#[tauri::command]
pub fn plugin_import(
    state: State<'_, Arc<ManagedState>>,
    paths: Vec<String>,
) -> CmdResult<Vec<PluginImportOutcome>> {
    Ok(paths.iter().map(|p| import_one(state.inner(), Path::new(p))).collect())
}

fn import_fail(path: &str, name: &str, status: &str, message: impl Into<String>) -> PluginImportOutcome {
    PluginImportOutcome {
        path: path.to_string(),
        name: name.to_string(),
        status: status.to_string(),
        message: Some(message.into()),
    }
}

fn import_one(state: &ManagedState, path: &Path) -> PluginImportOutcome {
    let path_string = path.to_string_lossy().to_string();
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')) {
        return import_fail(&path_string, &name, "error", format!("文件名 “{name}” 不能作为插件名"));
    }
    if matches!(state.db.get_plugin(&name), Ok(Some(_))) {
        return import_fail(&path_string, &name, "skipped", "同名插件已存在");
    }
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => return import_fail(&path_string, &name, "error", format!("读取失败: {e}")),
    };
    let mut scopes = extract_lua_string_list(&content, "scopes");
    if scopes.is_empty() {
        scopes = vec!["global".to_string()];
    }
    let mut capabilities = extract_lua_string_list(&content, "capabilities");
    if capabilities.is_empty() {
        capabilities = vec!["core".to_string()];
    }
    let rec = PluginRecord {
        name: name.clone(),
        source: "lua".to_string(),
        script_ref: format!("{name}.lua"),
        enabled: true,
        config: serde_json::Value::Null,
        scopes,
        capabilities,
    };
    if let Err(e) = state.db.upsert_plugin(&rec) {
        return import_fail(&path_string, &name, "error", format!("落库失败: {e}"));
    }
    if let Err(e) = write_script_inner(state, &name, &content) {
        // 脚本写入失败时回滚记录，避免留下空脚本插件
        let _ = state.db.delete_plugin(&name);
        return import_fail(&path_string, &name, "error", format!("脚本写入失败: {e}"));
    }
    PluginImportOutcome {
        path: path_string,
        name,
        status: "imported".to_string(),
        message: None,
    }
}

/// 列出某插件的全部作用域绑定。
#[tauri::command]
pub fn binding_list(state: State<'_, Arc<ManagedState>>, plugin_name: String) -> CmdResult<Vec<PluginBinding>> {
    Ok(state.db.list_bindings(&plugin_name)?)
}

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
