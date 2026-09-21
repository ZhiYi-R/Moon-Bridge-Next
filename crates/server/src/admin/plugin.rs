//! Plugin（Lua 插件）与 PluginBinding（作用域绑定）handlers。
//!
//! 注意：插件在网关 `bootstrap` 时加载为 `PluginHooks`，因此增删改插件后需调用
//! `POST /api/gateway/restart` 方能生效（前端在保存后触发）。
//!
//! 脚本路径一律经 [`script_file`] 收敛到 `plugins_dir` 内，越界即 400——切断
//! 「把 `script_ref` 写成任意绝对路径 → 任意文件读写」这条逃逸路。

use std::path::{Path, PathBuf};

use axum::extract::{Path as AxumPath, Query, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::Json;
use moonbridge_gateway::{parse_script_ref, PluginLuaRuntime, ScriptRef};
use moonbridge_store::{PluginBinding, PluginRecord};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{AdminState, ApiError, ApiResult};

/// 启用门控错误前缀：前端据此弹出「设置不满足」提示框而非普通错误横幅。
pub const REQUIREMENTS_ERROR_PREFIX: &str = "REQUIREMENTS";

/// 解析脚本引用为**受约束**的文件路径；`Ok(None)` 表示内联脚本。
///
/// 判定与网关加载侧共用 [`parse_script_ref`]（以 `.lua` 结尾 = 文件脚本），并强制
/// 相对路径归一到 `plugins_dir`、绝对路径必须落在 `plugins_dir` 之内。
fn script_file(state: &AdminState, script_ref: &str) -> ApiResult<Option<PathBuf>> {
    match parse_script_ref(script_ref, Some(state.paths.plugins_dir.as_path())) {
        ScriptRef::File(p) => Ok(Some(p)),
        ScriptRef::Inline(_) => Ok(None),
        ScriptRef::Rejected(p) => Err(ApiError::bad_request(format!(
            "插件脚本路径越出插件目录: {p}"
        ))),
    }
}

/// 列出全部插件。
pub async fn plugin_list(State(state): State<AdminState>) -> ApiResult<Json<Vec<PluginRecord>>> {
    Ok(Json(state.db.list_plugins()?))
}

/// 按名获取插件（不存在 404）。
pub async fn plugin_get(
    State(state): State<AdminState>,
    AxumPath(name): AxumPath<String>,
) -> ApiResult<Json<PluginRecord>> {
    let p = state
        .db
        .get_plugin(&name)?
        .ok_or_else(|| ApiError::not_found(format!("插件不存在: {name}")))?;
    Ok(Json(p))
}

/// 新增或更新插件记录。
///
/// 启用动作（新建即启用 / 停用 → 启用）会校验插件 `MB.requires` 声明的网关设置，
/// 不满足时以 400 + [`REQUIREMENTS_ERROR_PREFIX`] 开头的消息拒绝——前端弹提示框，
/// 不自动修改设置。
pub async fn plugin_save(
    State(state): State<AdminState>,
    Json(plugin): Json<PluginRecord>,
) -> ApiResult<Json<Value>> {
    // 脚本可读取时，以脚本 `MB` 清单为准提取 category/config_schema（清单是权威
    // 元数据，表单值只是兜底）；沙箱求值失败（如脚本语法错误）则保留表单值。
    let script = match script_file(&state, &plugin.script_ref)? {
        Some(f) if f.is_file() => {
            std::fs::read_to_string(&f).map_err(|e| ApiError::internal(e.to_string()))?
        }
        Some(_) => String::new(),          // 文件脚本尚未落盘
        None => plugin.script_ref.clone(), // 内联脚本即内容
    };
    let mut plugin = plugin;
    if !script.trim().is_empty() {
        if let Ok(manifest) = PluginLuaRuntime::manifest_of_script(&plugin.name, &script) {
            plugin.category = manifest.category;
            plugin.config_schema = manifest.config_schema.unwrap_or(Value::Null);
        }
    }
    let old_enabled = state
        .db
        .get_plugin(&plugin.name)?
        .map(|p| p.enabled)
        .unwrap_or(false);
    if plugin.enabled && !old_enabled {
        let cfg = serde_json::to_value(state.config().gateway)
            .map_err(|e| ApiError::internal(e.to_string()))?;
        let unmet = unmet_requirements(&cfg, &extract_lua_requires(&script));
        if !unmet.is_empty() {
            return Err(ApiError::bad_request(format!(
                "{REQUIREMENTS_ERROR_PREFIX}\n{}",
                unmet.join("\n")
            )));
        }
    }
    state.db.upsert_plugin(&plugin)?;
    Ok(Json(Value::Null))
}

/// 删除插件。
pub async fn plugin_delete(
    State(state): State<AdminState>,
    AxumPath(name): AxumPath<String>,
) -> ApiResult<Json<Value>> {
    state.db.delete_plugin(&name)?;
    Ok(Json(Value::Null))
}

/// 读取插件脚本内容：`.lua` 文件脚本读文件（相对路径归一到 `plugins_dir`），
/// 否则视为内联脚本直接返回 `script_ref`。
pub async fn plugin_read_script(
    State(state): State<AdminState>,
    AxumPath(name): AxumPath<String>,
) -> ApiResult<Response> {
    let rec = state
        .db
        .get_plugin(&name)?
        .ok_or_else(|| ApiError::not_found(format!("插件不存在: {name}")))?;
    let text = match script_file(&state, &rec.script_ref)? {
        Some(file) => {
            if file.is_file() {
                std::fs::read_to_string(&file).map_err(|e| ApiError::internal(e.to_string()))?
            } else {
                String::new() // 文件脚本但尚未落盘
            }
        }
        None => rec.script_ref, // 内联脚本
    };
    Ok(([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], text).into_response())
}

/// 写入（保存）插件脚本内容，供前端在线编辑。
///
/// 请求体为 `text/plain` 原样脚本。文件脚本（`.lua`）：写入**已收敛到 `plugins_dir`
/// 内**的路径（必要时创建父目录），并把 `script_ref` 规范化为该绝对路径，确保网关按
/// 同一路径读取；内联脚本：直接写回记录。
pub async fn plugin_write_script(
    State(state): State<AdminState>,
    AxumPath(name): AxumPath<String>,
    body: String,
) -> ApiResult<Json<Value>> {
    write_script_inner(&state, &name, &body)?;
    Ok(Json(Value::Null))
}

/// [`plugin_write_script`] 的内部实现，供导入流程复用。
fn write_script_inner(state: &AdminState, name: &str, content: &str) -> ApiResult<()> {
    let mut rec = state
        .db
        .get_plugin(name)?
        .ok_or_else(|| ApiError::not_found(format!("插件不存在: {name}")))?;
    match script_file(state, &rec.script_ref)? {
        Some(file) => {
            if let Some(parent) = file.parent() {
                std::fs::create_dir_all(parent).map_err(|e| ApiError::internal(e.to_string()))?;
            }
            std::fs::write(&file, content).map_err(|e| ApiError::internal(e.to_string()))?;
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

/// 从 Lua 脚本中尽力提取 `key = "value"` 形式的标量字符串（导入时读取 `category`
/// 声明）；找不到回退 `None`。与 [`extract_lua_string_list`] 同一取舍：忽略转义与注释。
fn extract_lua_string(script: &str, key: &str) -> Option<String> {
    let pos = script.find(key)?;
    let rest = &script[pos + key.len()..];
    let eq = rest.find('=')?;
    let open = rest[eq..].find('"')?;
    let close = rest[eq + open + 1..].find('"')?;
    Some(rest[eq + open + 1..eq + open + 1 + close].to_string())
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
    let Some(rest) = cleaned[pos + "requires".len()..]
        .trim_start()
        .strip_prefix('=')
    else {
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
            serde_json::Number::from_f64(n)
                .map(Value::Number)
                .unwrap_or(Value::Null)
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

/// 导入请求体：`{"files":[{"name":"x.lua","content":"..."}]}`。
#[derive(Debug, Deserialize)]
pub struct PluginImportRequest {
    pub files: Vec<PluginImportFile>,
}

/// 待导入的单个插件文件。
#[derive(Debug, Deserialize)]
pub struct PluginImportFile {
    /// 文件名（须以 `.lua` 结尾，插件名取出去扩展名的部分）。
    pub name: String,
    /// 脚本内容。
    #[serde(default)]
    pub content: String,
}

/// 从请求体内容导入 `.lua` 插件：以文件名（去扩展名）为插件名，脚本写入
/// `plugins_dir/{name}.lua` 并落库；同名插件已存在时跳过（不覆盖）。scopes/capabilities
/// 尽力从脚本 `MB = { scopes = {...}, capabilities = {...} }` 声明提取，缺省
/// global/core。逐文件返回结果，单个失败不影响其余。
pub async fn plugin_import(
    State(state): State<AdminState>,
    Json(req): Json<PluginImportRequest>,
) -> ApiResult<Json<Vec<PluginImportOutcome>>> {
    Ok(Json(
        req.files.iter().map(|f| import_one(&state, f)).collect(),
    ))
}

fn import_fail(
    path: &str,
    name: &str,
    status: &str,
    message: impl Into<String>,
) -> PluginImportOutcome {
    PluginImportOutcome {
        path: path.to_string(),
        name: name.to_string(),
        status: status.to_string(),
        message: Some(message.into()),
    }
}

fn import_one(state: &AdminState, file: &PluginImportFile) -> PluginImportOutcome {
    // 与桌面端同一口径：`path` 字段回填文件名，插件名取去扩展名的 stem。
    let path_string = file.name.clone();
    let name = Path::new(&file.name)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
    {
        return import_fail(
            &path_string,
            &name,
            "error",
            format!("文件名 “{name}” 不能作为插件名"),
        );
    }
    if matches!(state.db.get_plugin(&name), Ok(Some(_))) {
        return import_fail(&path_string, &name, "skipped", "同名插件已存在");
    }
    let content = file.content.clone();
    let mut scopes = extract_lua_string_list(&content, "scopes");
    if scopes.is_empty() {
        scopes = vec!["global".to_string()];
    }
    let mut capabilities = extract_lua_string_list(&content, "capabilities");
    if capabilities.is_empty() {
        capabilities = vec!["core".to_string()];
    }
    // 沙箱求值 `MB` 清单拿 category/config_schema；求值失败回退文本提取的 category。
    let manifest = PluginLuaRuntime::manifest_of_script(&name, &content).ok();
    let category = manifest
        .as_ref()
        .map(|m| m.category.clone())
        .or_else(|| extract_lua_string(&content, "category"))
        .unwrap_or_else(|| "core".to_string());
    let config_schema = manifest
        .and_then(|m| m.config_schema)
        .unwrap_or(Value::Null);
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
        category,
        config_schema,
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
            Some(format!(
                "已导入但保持停用：{}；可在设置中调整后启用",
                unmet.join("；")
            ))
        },
    }
}

/// 列出某插件的全部作用域绑定。
pub async fn binding_list(
    State(state): State<AdminState>,
    AxumPath(name): AxumPath<String>,
) -> ApiResult<Json<Vec<PluginBinding>>> {
    Ok(Json(state.db.list_bindings(&name)?))
}

pub async fn binding_save(
    State(state): State<AdminState>,
    Json(binding): Json<PluginBinding>,
) -> ApiResult<Json<Value>> {
    state.db.upsert_binding(&binding)?;
    Ok(Json(Value::Null))
}

/// `GET /api/bindings?scope=` 的查询参数（scope：provider/model/route/global）。
#[derive(Debug, Deserialize)]
pub struct ScopeParams {
    pub scope: String,
}

/// 列出某一作用域类型的全部绑定。
pub async fn binding_list_by_scope(
    State(state): State<AdminState>,
    Query(params): Query<ScopeParams>,
) -> ApiResult<Json<Vec<PluginBinding>>> {
    Ok(Json(state.db.list_bindings_by_scope(&params.scope)?))
}

/// 删除作用域绑定（三态开关重置回「跟随全局」时调用）。
pub async fn binding_delete(
    State(state): State<AdminState>,
    AxumPath((plugin_name, scope, scope_key)): AxumPath<(String, String, String)>,
) -> ApiResult<Json<Value>> {
    state.db.delete_binding(&plugin_name, &scope, &scope_key)?;
    Ok(Json(Value::Null))
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
        let unmet = unmet_requirements(
            &json!({ "sessionMarker": false }),
            &[("sessionMarker".into(), json!(true))],
        );
        assert_eq!(unmet.len(), 1);
        assert!(unmet[0].contains("会话水印"), "{}", unmet[0]);
        assert!(unmet[0].contains("开启") && unmet[0].contains("关闭"));
        // 满足：空
        assert!(unmet_requirements(
            &json!({ "sessionMarker": true }),
            &[("sessionMarker".into(), json!(true))]
        )
        .is_empty());
        // 配置缺字段视为不满足
        assert_eq!(
            unmet_requirements(&json!({}), &[("nope".into(), json!(true))]).len(),
            1
        );
    }

    #[test]
    fn extracts_scope_and_capability_lists() {
        let script = r#"
MB = {
  scopes = { "global", "provider" },
  capabilities = { "core", "raw_request" },
}
"#;
        assert_eq!(
            extract_lua_string_list(script, "scopes"),
            vec!["global", "provider"]
        );
        assert_eq!(
            extract_lua_string_list(script, "capabilities"),
            vec!["core", "raw_request"]
        );
        // 找不到 key 或列表畸形时回退空表
        assert!(extract_lua_string_list(script, "missing").is_empty());
        assert!(extract_lua_string_list("scopes = {", "scopes").is_empty());
    }
}
