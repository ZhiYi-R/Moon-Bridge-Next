//! Plugin（Lua 插件）与 PluginBinding（作用域绑定）CRUD commands。
//!
//! 注意：插件在网关 `bootstrap` 时加载为 `PluginHooks`，因此增删改插件后需
//! 调用 `gateway_restart` 方能生效（前端在保存后触发）。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use moonbridge_gateway::{parse_script_ref, ScriptRef};
use moonbridge_store::{PluginBinding, PluginRecord};
use serde::Serialize;
use serde_json::Value;
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
///
/// 启用动作（新建即启用 / 停用 → 启用）会校验插件 `MB.requires` 声明的网关设置，
/// 不满足时拒绝并返回 [`REQUIREMENTS_ERROR_PREFIX`] 开头的结构化错误——前端弹提示框，
/// 不自动修改设置。
#[tauri::command]
pub fn plugin_save(state: State<'_, Arc<ManagedState>>, plugin: PluginRecord) -> CmdResult<()> {
    let old_enabled = state
        .db
        .get_plugin(&plugin.name)?
        .map(|p| p.enabled)
        .unwrap_or(false);
    if plugin.enabled && !old_enabled {
        let script = match script_file(&state, &plugin.script_ref)? {
            Some(f) if f.is_file() => std::fs::read_to_string(&f).map_err(|e| e.to_string())?,
            Some(_) => String::new(), // 文件脚本尚未落盘
            None => plugin.script_ref.clone(), // 内联脚本即内容
        };
        let cfg = serde_json::to_value(state.config().gateway).map_err(|e| e.to_string())?;
        let unmet = unmet_requirements(&cfg, &extract_lua_requires(&script));
        if !unmet.is_empty() {
            return Err(format!("{REQUIREMENTS_ERROR_PREFIX}\n{}", unmet.join("\n")).into());
        }
    }
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

/// 启用门控：从 Lua 脚本尽力提取 `MB.requires = { key = value, ... }` 声明。
///
/// 键为网关配置（GatewayConfig）的 camelCase 字段名，值支持 true/false/整数/浮点数/
/// 带引号字符串。行级剥离 `--` 注释后匹配，与 [`extract_lua_string_list`] 同一取舍。
fn extract_lua_requires(script: &str) -> Vec<(String, Value)> {
    let cleaned = script
        .lines()
        .map(|l| match l.find("--") {
            Some(i) => &l[..i],
            None => l,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mut out = Vec::new();
    let Some(pos) = cleaned.find("requires") else {
        return out;
    };
    let Some(rest) = cleaned[pos + "requires".len()..].trim_start().strip_prefix('=') else {
        return out;
    };
    let Some(open) = rest.find('{') else {
        return out;
    };
    let rest = &rest[open..];
    let Some(close) = rest.find('}') else {
        return out;
    };
    for item in rest[1..close].split(',') {
        let item = item.trim();
        let Some(eq) = item.find('=') else {
            continue;
        };
        let key = item[..eq].trim().trim_matches('"').to_string();
        let raw = item[eq + 1..].trim();
        let value = if raw == "true" {
            Value::Bool(true)
        } else if raw == "false" {
            Value::Bool(false)
        } else if let Ok(n) = raw.parse::<i64>() {
            Value::Number(n.into())
        } else if let Ok(n) = raw.parse::<f64>() {
            serde_json::Number::from_f64(n).map(Value::Number).unwrap_or(Value::Null)
        } else {
            Value::String(raw.trim_matches(|c| c == '"' || c == '\'').to_string())
        };
        if !key.is_empty() {
            out.push((key, value));
        }
    }
    out
}

/// 校验消息里设置的中文别名（缺省回退字段名）。
fn setting_label(key: &str) -> &str {
    match key {
        "sessionMarker" => "会话水印",
        "traceRecordBodies" => "trace 记录请求/响应体",
        _ => key,
    }
}

/// 值的可读形式：bool 显示 开启/关闭，其余显示 JSON。
fn fmt_setting(v: &Value) -> String {
    match v {
        Value::Bool(true) => "开启".to_string(),
        Value::Bool(false) => "关闭".to_string(),
        other => other.to_string(),
    }
}

/// 对照当前网关配置计算未满足项；空 = 全部满足。
fn unmet_requirements(cfg: &Value, requires: &[(String, Value)]) -> Vec<String> {
    requires
        .iter()
        .filter(|(k, want)| cfg.get(k.as_str()) != Some(want))
        .map(|(k, want)| {
            let cur = cfg.get(k).unwrap_or(&Value::Null);
            format!(
                "「{}」需为 {}，当前 {}",
                setting_label(k),
                fmt_setting(want),
                fmt_setting(cur)
            )
        })
        .collect()
}

/// 启用门控错误前缀：前端据此弹出「设置不满足」提示框而非普通错误横幅。
pub const REQUIREMENTS_ERROR_PREFIX: &str = "REQUIREMENTS";

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
    // 导入即启用，但 `MB.requires` 未满足时保持停用（在结果 message 说明，不阻断导入）；
    // 配置序列化失败时视为不校验（保持启用，不阻断导入）
    let cfg = serde_json::to_value(state.config().gateway).unwrap_or(Value::Null);
    let unmet = if cfg.is_null() {
        Vec::new()
    } else {
        unmet_requirements(&cfg, &extract_lua_requires(&content))
    };
    let rec = PluginRecord {
        name: name.clone(),
        source: "lua".to_string(),
        script_ref: format!("{name}.lua"),
        enabled: unmet.is_empty(),
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
        message: if unmet.is_empty() {
            None
        } else {
            Some(format!("已导入但保持停用：{}；可在设置中调整后启用", unmet.join("；")))
        },
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extracts_requires_with_types_and_strips_comments() {
        let script = r#"
MB = {
  version = "0.1.0",
  -- requires = { fake = true } 注释里的声明不提取
  requires = { sessionMarker = true, logLevel = "debug", maxBodyBytes = 1024, ratio = 0.5 },
}
"#;
        let reqs = extract_lua_requires(script);
        assert_eq!(reqs[0], ("sessionMarker".to_string(), json!(true)));
        assert_eq!(reqs[1], ("logLevel".to_string(), json!("debug")));
        assert_eq!(reqs[2], ("maxBodyBytes".to_string(), json!(1024)));
        assert_eq!(reqs[3], ("ratio".to_string(), json!(0.5)));
    }

    #[test]
    fn no_requires_or_malformed_yields_empty() {
        assert!(extract_lua_requires("MB = { version = '0.1.0' }").is_empty());
        assert!(extract_lua_requires("local requires = 42").is_empty());
        assert!(extract_lua_requires("requires = { broken").is_empty());
    }

    #[test]
    fn unmet_requirements_reports_current_and_expected() {
        // 未满足：消息含中文别名与双方值
        let unmet = unmet_requirements(&json!({ "sessionMarker": false }), &[("sessionMarker".into(), json!(true))]);
        assert_eq!(unmet.len(), 1);
        assert!(unmet[0].contains("会话水印"), "{}", unmet[0]);
        assert!(unmet[0].contains("开启") && unmet[0].contains("关闭"));
        // 满足：空
        assert!(unmet_requirements(&json!({ "sessionMarker": true }), &[("sessionMarker".into(), json!(true))]).is_empty());
        // 配置缺字段视为不满足
        assert_eq!(unmet_requirements(&json!({}), &[("nope".into(), json!(true))]).len(), 1);
    }
}
