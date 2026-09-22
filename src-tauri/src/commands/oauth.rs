//! OAuth 登录编排 commands（**通用**：平台细节全部在 CAP_AUTH 插件内）。
//!
//! 入口模型与 quota 同构：宿主只提供能力与原语，登录目标由**插件/绑定**驱动——
//! `oauth_begin(provider)` 走 provider 已绑定的 auth 插件（重登录/手动绑定路径）；
//! `oauth_begin(plugin)` 由插件 `describe.provider` 模板新建 provider。
//! `oauth_list` 列出全部声明 `auth` 能力的插件（含 describe 元数据）供选择器渲染。
//!
//! 形态：`oauth_describe` 返回插件自述（流程类型/来源选项/是否支持粘贴/provider
//! 模板，供 UI 决定渲染与来源选择）→ `oauth_begin` 启动后台登录任务并返回流程
//! 描述 → 前端以 `oauth_status` 轮询、`oauth_cancel` 中止、`oauth_paste` 提交粘贴。
//!
//! 登录成功的令牌包：经 `oauth::write_bundle` 加密存 secrets 表（scope
//! `provider:{key}`）；provider 行只记非敏感账户元数据（`extra.auth`），端点
//! `api_key` 置空——出站凭据由 dispatch 经 `MB.auth_headers` 注入。随后写
//! provider 维度插件绑定（auth 插件对该 provider 生效）。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use moonbridge_gateway::bridge::GatewayBridge;
use moonbridge_gateway::oauth::{self, CallbackRegistry};
use moonbridge_plugin::{LuaRuntime, SandboxLimits};
use moonbridge_protocol::builtin_registry;
use moonbridge_store::{Database, Endpoint, PluginBinding, PluginRecord, Provider};
use serde::Serialize;
use serde_json::{json, Value};
use tauri::State;

use crate::commands::CmdResult;
use crate::state::{ManagedState, OAuthFlow, OAuthFlowStatus};

/// `oauth_describe` 的返回：插件名 + 插件自述原样（kind/label/instructions/
/// supports_paste/sources/provider 模板等由插件定义，UI 按元数据渲染）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthDescribe {
    pub plugin: String,
    pub describe: Value,
}

/// `oauth_list` 的条目：声明 `auth` 能力的插件 + 其 describe 元数据（失败时
/// `describe` 为 None、`error` 记原因——坏插件不阻断列表）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthPluginEntry {
    pub plugin: String,
    pub describe: Option<Value>,
    pub error: Option<String>,
}

/// `oauth_begin` 的返回。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthBegin {
    pub flow_id: String,
    /// 流程类型（describe.kind：device_code / callback）。
    pub kind: String,
    /// begin 阶段已完成（本地凭据导入命中，无后台任务）。
    pub already_done: bool,
    /// 需要用户打开的页面（设备验证页 / 授权页）。
    pub verification_url: Option<String>,
    /// 设备码（device_code 流展示给用户输入）。
    pub user_code: Option<String>,
    pub instructions: Option<String>,
    /// 提示（如「未检测到本地凭据，已改用浏览器登录」）。
    pub notice: Option<String>,
    /// 是否展示粘贴输入框（describe.supports_paste）。
    pub supports_paste: bool,
    /// already_done 时创建/更新的 provider key。
    pub provider_key: Option<String>,
}

/// 认证插件解析：存在且声明 `auth` 能力（capability 是唯一门槛，插件名不特判）。
fn auth_plugin_record(db: &Database, name: &str) -> CmdResult<PluginRecord> {
    let p = db
        .get_plugin(name)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("认证插件不存在: {name}"))?;
    if !p.capabilities.iter().any(|c| c == "auth") {
        return Err(format!("插件 {name} 未声明 auth 能力").into());
    }
    Ok(p)
}

/// provider 维度已绑定且启用的 auth 插件（provider 路径登录的插件来源——
/// 与运行时 `auth_plugin_for` 同口径：只看显式 provider 绑定）。
fn bound_auth_plugin(db: &Database, provider_key: &str) -> CmdResult<String> {
    let bindings = db
        .list_bindings_by_scope("provider")
        .map_err(|e| e.to_string())?;
    let mut bound = false;
    for b in bindings
        .iter()
        .filter(|b| b.scope_key == provider_key && b.enabled)
    {
        bound = true;
        if db
            .get_plugin(&b.plugin_name)
            .ok()
            .flatten()
            .is_some_and(|p| p.capabilities.iter().any(|c| c == "auth"))
        {
            return Ok(b.plugin_name.clone());
        }
    }
    Err(if bound {
        format!("上游 {provider_key} 绑定的插件均未声明 auth 能力")
    } else {
        format!("上游 {provider_key} 未绑定认证插件")
    }
    .into())
}

/// 登录流程的运行环境：一次性插件运行时 + 本流程私有的回调监听注册表
/// （流程结束/取消即随作用域释放，listener 在 CallbackRegistry::drop 清理）。
fn flow_runtime(
    st: &Arc<ManagedState>,
    plugin: &str,
) -> Result<(Arc<LuaRuntime>, Arc<CallbackRegistry>), String> {
    let callbacks = CallbackRegistry::new();
    // 登录子请求：不跟随重定向（凭据体不得被重放到 Location 主机）；
    // 单请求超时由 mb.http 的 30s 兜底施加
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| format!("构建 HTTP 客户端失败: {e}"))?;
    let bridge = Arc::new(GatewayBridge::new(
        client.clone(),
        client,
        st.db.clone(),
        Arc::new(builtin_registry()),
        callbacks.clone(),
        Duration::from_secs(30),
    ));
    // 单次调用上限放宽到 90s：回调心跳 + 平台 http 往返的宽松预算
    let limits = SandboxLimits {
        call_timeout: Duration::from_secs(90),
        ..SandboxLimits::default()
    };
    let rt = oauth::load_plugin_runtime(
        &st.db,
        plugin,
        bridge,
        Some(st.paths.plugins_dir.as_path()),
        limits,
    )?;
    Ok((rt, callbacks))
}

/// 登录成功落库：令牌包 → secrets 表；provider 行只记非敏感 `extra.auth`。
///
/// - `existing_provider`（provider 路径）：provider 已存在，仅刷新 extra.auth
///   元数据与令牌包——端点/表单字段保持原样（用户可改过 base_url 等）。
/// - `None`（插件路径）：按插件 `describe.provider` 模板建 provider（端点
///   `api_key` 置空，出站凭据由 `MB.auth_headers` 注入）并写 provider 维度绑定。
///   同名冲突只允许覆盖同插件创建的 OAuth 账户（手建 provider 不被登录吞掉）。
fn write_auth_provider(
    db: &Database,
    plugin: &str,
    existing_provider: Option<&str>,
    describe: &Value,
    bundle: &Value,
) -> Result<String, String> {
    oauth::validate_bundle(bundle)?;
    let template = describe.get("provider");
    let key = existing_provider
        .map(str::to_string)
        .or_else(|| {
            template
                .and_then(|t| t.get("key"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .ok_or_else(|| format!("插件 {plugin} 的 describe.provider 未声明 key（新建路径必需）"))?;
    let mut created_at = 0;
    if let Some(old) = db.get_provider(&key).map_err(|e| e.to_string())? {
        // 同名冲突检查只作用于新建路径（provider 路径的绑定本身是显式授权）
        if existing_provider.is_none() {
            let old_plugin = old
                .extra
                .get("auth")
                .and_then(|a| a.get("plugin"))
                .and_then(Value::as_str);
            if old_plugin != Some(plugin) {
                return Err(format!(
                    "上游 “{key}” 已存在且不是同名 OAuth 账户，请先删除"
                ));
            }
        }
        created_at = old.created_at;
    }
    // 令牌包加密存 secrets 表（不进 provider.extra、不随 provider_get 回传前端）
    oauth::write_bundle(db, &key, bundle)?;
    let meta = json!({
        "plugin": plugin,
        "account": bundle.get("account"),
        "email": bundle.get("email"),
        "source": bundle.get("source"),
        // models.dev 目录 id 由插件模板声明（detect 的 enrich/回退数据源）
        "models_dev_id": template.and_then(|t| t.get("models_dev_id")).cloned().unwrap_or(Value::Null),
    });
    // provider 路径：只刷新元数据，provider 行其余字段（端点/配额/开关）保持原样
    if existing_provider.is_some() {
        let mut p = db
            .get_provider(&key)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("上游 {key} 在登录期间被删除"))?;
        p.extra["auth"] = meta;
        db.upsert_provider(&p).map_err(|e| e.to_string())?;
        return Ok(key);
    }
    let t = template
        .ok_or_else(|| format!("插件 {plugin} 的 describe 未声明 provider 模板（新建路径必需）"))?;
    let protocol = t
        .get("protocol")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("插件 {plugin} 的 describe.provider 缺 protocol"))?;
    let base_url = t
        .get("base_url")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("插件 {plugin} 的 describe.provider 缺 base_url"))?;
    let p = Provider {
        key: key.clone(),
        endpoints: vec![Endpoint {
            protocol: protocol.to_string(),
            base_url: base_url.to_string(),
            api_key: String::new(),
        }],
        version: None,
        user_agent: None,
        web_search: None,
        extra: json!({ "auth": meta }),
        enabled: true,
        quota_plugin_ref: String::new(),
        quota_interval_secs: 0,
        quota_enabled: false,
        quota_config: Value::Null,
        created_at,
        updated_at: 0,
    };
    db.upsert_provider(&p).map_err(|e| e.to_string())?;
    db.upsert_binding(&PluginBinding {
        plugin_name: plugin.to_string(),
        scope: "provider".to_string(),
        scope_key: key.clone(),
        enabled: true,
        config: Value::Null,
    })
    .map_err(|e| e.to_string())?;
    Ok(key)
}

/// 流程收尾：结果写入共享状态（done 带 provider key / error 带原因）。
fn finish_flow(result: Result<String, String>, status: &Mutex<OAuthFlowStatus>) {
    let mut g = status.lock().unwrap();
    match result {
        Ok(key) => {
            g.state = "done".to_string();
            g.provider_key = Some(key);
            g.message = None;
        }
        Err(e) => {
            g.state = "error".to_string();
            g.message = Some(e);
        }
    }
}

/// 粘贴无效/进度提示（流程保持 pending，可继续粘贴或等回调）。
fn set_pending_msg(status: &Mutex<OAuthFlowStatus>, msg: String) {
    let mut g = status.lock().unwrap();
    g.state = "pending".to_string();
    g.message = Some(msg);
}

/// 列出全部声明 `auth` 能力的插件及其 describe 元数据（预设选择器账户组的数据源；
/// 单个插件 describe 失败不阻断列表——error 字段带给前端）。
#[tauri::command]
pub async fn oauth_list(state: State<'_, Arc<ManagedState>>) -> CmdResult<Vec<OAuthPluginEntry>> {
    let st = state.inner().clone();
    let plugins = st.db.list_plugins().map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for p in plugins
        .into_iter()
        .filter(|p| p.capabilities.iter().any(|c| c == "auth"))
    {
        let (describe, error) = match flow_runtime(&st, &p.name) {
            Ok((rt, _callbacks)) => match rt
                .call_mb_once("auth_describe", &oauth::auth_ctx(&p.name))
                .await
            {
                Ok(d) => (Some(d), None),
                Err(e) => (None, Some(e.to_string())),
            },
            Err(e) => (None, Some(e)),
        };
        out.push(OAuthPluginEntry {
            plugin: p.name,
            describe,
            error,
        });
    }
    Ok(out)
}

/// 目标解析（describe/begin 共用）：provider 路径走其绑定的 auth 插件；
/// plugin 路径校验能力声明。返回 `(插件名, 已有 provider key)`。
fn resolve_target(
    db: &Database,
    plugin: Option<&str>,
    provider: Option<&str>,
) -> CmdResult<(String, Option<String>)> {
    match provider {
        Some(key) => {
            if db.get_provider(key).map_err(|e| e.to_string())?.is_none() {
                return Err(format!("上游不存在: {key}").into());
            }
            Ok((bound_auth_plugin(db, key)?, Some(key.to_string())))
        }
        None => {
            let name = plugin.ok_or_else(|| "需要 plugin 或 provider 参数".to_string())?;
            auth_plugin_record(db, name)?;
            Ok((name.to_string(), None))
        }
    }
}

/// 插件自述（UI 先调它决定渲染与来源选择，再调 `oauth_begin`）。
/// 目标同 `oauth_begin`：`provider` 重登录走绑定插件，`plugin` 走新建路径。
#[tauri::command]
pub async fn oauth_describe(
    state: State<'_, Arc<ManagedState>>,
    plugin: Option<String>,
    provider: Option<String>,
) -> CmdResult<OAuthDescribe> {
    let st = state.inner().clone();
    let (plugin_name, key) = resolve_target(&st.db, plugin.as_deref(), provider.as_deref())?;
    let (rt, _callbacks) = flow_runtime(&st, &plugin_name)?;
    let ctx_provider = key.unwrap_or_else(|| plugin_name.clone());
    let describe = rt
        .call_mb_once("auth_describe", &oauth::auth_ctx(&ctx_provider))
        .await
        .map_err(|e| format!("auth_describe 调用失败: {e}"))?;
    Ok(OAuthDescribe {
        plugin: plugin_name,
        describe,
    })
}

/// 启动登录流程（`source` 为 UI 传来的来源选择；describe.sources 未提供时传 None）。
///
/// 二选一目标：`provider` = 已存在 provider 的重登录（插件取其 provider 维度绑定）；
/// `plugin` = 新建路径（成功后按插件 describe.provider 模板建 provider）。
#[tauri::command]
pub async fn oauth_begin(
    state: State<'_, Arc<ManagedState>>,
    plugin: Option<String>,
    provider: Option<String>,
    source: Option<String>,
) -> CmdResult<OAuthBegin> {
    let st = state.inner().clone();
    run_begin(&st, plugin.as_deref(), provider.as_deref(), source).await
}

pub async fn run_begin(
    st: &Arc<ManagedState>,
    plugin: Option<&str>,
    provider: Option<&str>,
    source: Option<String>,
) -> CmdResult<OAuthBegin> {
    // 目标解析：provider 路径走其绑定的 auth 插件；plugin 路径走 describe 模板新建
    let (plugin_name, existing_key) = resolve_target(&st.db, plugin, provider)?;
    let (rt, callbacks) = flow_runtime(st, &plugin_name)?;
    // ctx.provider：已有 provider 用其 key；新建路径用插件名（模板建议 key 是
    // describe 的产出，调用前不可得；插件可拿它区分多账户等场景）
    let ctx_provider = existing_key.clone().unwrap_or_else(|| plugin_name.clone());
    let describe = rt
        .call_mb_once("auth_describe", &oauth::auth_ctx(&ctx_provider))
        .await
        .map_err(|e| format!("auth_describe 调用失败: {e}"))?;
    let kind = describe
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("callback")
        .to_string();
    let supports_paste = describe
        .get("supports_paste")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let instructions = describe
        .get("instructions")
        .and_then(Value::as_str)
        .map(str::to_string);

    let ctx = oauth::auth_ctx_with(&ctx_provider, source.as_deref());
    let begin = rt
        .call_mb_once("auth_begin", &ctx)
        .await
        .map_err(|e| format!("auth_begin 调用失败: {e}"))?;
    // begin 直出终态：done（本地导入命中）/ error（显式失败，如 local-cli 验证失败）
    match begin.get("status").and_then(Value::as_str) {
        Some("done") => {
            let key = write_auth_provider(
                &st.db,
                &plugin_name,
                existing_key.as_deref(),
                &describe,
                &begin["bundle"],
            )?;
            return Ok(OAuthBegin {
                flow_id: String::new(),
                kind,
                already_done: true,
                verification_url: None,
                user_code: None,
                instructions,
                notice: None,
                supports_paste,
                provider_key: Some(key),
            });
        }
        Some("error") => {
            let msg = begin
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("登录发起失败");
            return Err(msg.to_string().into());
        }
        _ => {}
    }

    let handle = begin.get("handle").cloned().unwrap_or(Value::Null);
    let verification_url = begin
        .get("verification_url")
        .and_then(Value::as_str)
        .map(str::to_string);
    let user_code = begin
        .get("user_code")
        .and_then(Value::as_str)
        .map(str::to_string);
    let notice = begin
        .get("notice")
        .and_then(Value::as_str)
        .map(str::to_string);
    let interval_secs = begin
        .get("interval_secs")
        .and_then(Value::as_u64)
        .unwrap_or(2)
        .clamp(1, 30);
    let expires_in_secs = begin
        .get("expires_in_secs")
        .and_then(Value::as_u64)
        .unwrap_or(120)
        .clamp(30, 1800);

    let flow_id = uuid::Uuid::new_v4().simple().to_string();
    let (abort_tx, mut abort_rx) = tokio::sync::oneshot::channel::<()>();
    let (paste_tx, mut paste_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let status = Arc::new(Mutex::new(OAuthFlowStatus::pending()));
    let status2 = status.clone();
    let db = st.db.clone();
    let plugin_move = plugin_name.clone();
    tokio::spawn(async move {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(expires_in_secs);
        let mut interval = Duration::from_secs(interval_secs);
        let result: Result<String, String> = loop {
            tokio::select! {
                _ = tokio::time::sleep(interval) => {
                    // 多条粘贴只留最新一条
                    let mut paste: Option<String> = None;
                    while let Ok(t) = paste_rx.try_recv() {
                        paste = Some(t);
                    }
                    let paste_v = paste.map(Value::String).unwrap_or(Value::Null);
                    match rt.call_mb("auth_poll", &[ctx.clone(), handle.clone(), paste_v]).await {
                        Ok(v) => {
                            let st_str = v.get("status").and_then(Value::as_str).unwrap_or("");
                            match st_str {
                                "done" => break write_auth_provider(
                                    &db,
                                    &plugin_move,
                                    existing_key.as_deref(),
                                    &describe,
                                    &v["bundle"],
                                ),
                                "pending" => {
                                    if let Some(m) = v
                                        .get("message")
                                        .and_then(Value::as_str)
                                        .filter(|s| !s.is_empty())
                                    {
                                        set_pending_msg(&status2, m.to_string());
                                    }
                                }
                                "slow_down" => {
                                    let iv = v
                                        .get("interval_secs")
                                        .and_then(Value::as_u64)
                                        .unwrap_or(10);
                                    interval = Duration::from_secs(iv.clamp(1, 60));
                                }
                                "expired" => {
                                    break Err(v
                                        .get("message")
                                        .and_then(Value::as_str)
                                        .unwrap_or("登录已过期，请重新发起")
                                        .to_string())
                                }
                                "error" => {
                                    break Err(v
                                        .get("message")
                                        .and_then(Value::as_str)
                                        .unwrap_or("登录失败")
                                        .to_string())
                                }
                                other => break Err(format!("插件返回未知状态: {other}")),
                            }
                        }
                        Err(e) => break Err(format!("auth_poll 调用失败: {e}")),
                    }
                }
                _ = &mut abort_rx => break Err("登录已取消".to_string()),
                _ = tokio::time::sleep_until(deadline) => {
                    break Err("登录超时，请重新发起".to_string())
                }
            }
        };
        finish_flow(result, &status2);
        // rt / callbacks 随作用域释放：listener 在 CallbackRegistry::drop 中清理
        drop(rt);
        drop(callbacks);
    });
    st.oauth_flows.lock().unwrap().insert(
        flow_id.clone(),
        OAuthFlow {
            status,
            abort: abort_tx,
            paste: paste_tx,
        },
    );
    Ok(OAuthBegin {
        flow_id,
        kind,
        already_done: false,
        verification_url,
        user_code,
        instructions,
        notice,
        supports_paste,
        provider_key: None,
    })
}

/// 查询登录流程状态。
#[tauri::command]
pub fn oauth_status(
    state: State<'_, Arc<ManagedState>>,
    flow_id: String,
) -> CmdResult<OAuthFlowStatus> {
    let flows = state.oauth_flows.lock().unwrap();
    let Some(f) = flows.get(&flow_id) else {
        return Err(format!("登录流程不存在或已结束: {flow_id}").into());
    };
    let status = f.status.lock().unwrap().clone();
    Ok(status)
}

/// 中止登录流程（移除即中止：sender Drop 使后台 select 的取消分支生效）。
#[tauri::command]
pub fn oauth_cancel(state: State<'_, Arc<ManagedState>>, flow_id: String) -> CmdResult<()> {
    if let Some(flow) = state.oauth_flows.lock().unwrap().remove(&flow_id) {
        drop(flow.abort);
        drop(flow.paste);
    }
    Ok(())
}

/// 提交手动粘贴（透传给下一轮 auth_poll；插件自行解析与校验）。
#[tauri::command]
pub fn oauth_paste(
    state: State<'_, Arc<ManagedState>>,
    flow_id: String,
    text: String,
) -> CmdResult<()> {
    let flows = state.oauth_flows.lock().unwrap();
    let f = flows
        .get(&flow_id)
        .ok_or_else(|| format!("登录流程不存在或已结束: {flow_id}"))?;
    f.paste
        .send(text)
        .map_err(|_| "登录流程已结束".to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppPaths;

    fn temp_state(tag: &str) -> (Arc<ManagedState>, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("mb-oauth-cmd-{}-{tag}", std::process::id()));
        let paths = AppPaths::resolve(dir.join("config"), dir.join("data"));
        let st = ManagedState::new(paths).unwrap();
        (st, dir)
    }

    /// 插件 describe 的 provider 模板（等价于 Lua 侧 describe.provider 表）。
    fn describe_tpl(key: &str, base_url: &str) -> Value {
        json!({
            "kind": "callback",
            "provider": {
                "key": key,
                "label": key,
                "protocol": "openai-chat",
                "base_url": base_url,
            }
        })
    }

    #[test]
    fn write_auth_provider_layout() {
        let db = Database::open_in_memory().unwrap();
        let bundle = json!({
            "access": "tok-1", "refresh": "ref-1", "expires_at": 123,
            "account": "u1", "email": "a@b.com", "source": "oauth",
        });
        let desc = describe_tpl(
            "command-code-auth",
            "https://api.commandcode.ai/provider/v1",
        );
        let key = write_auth_provider(&db, "auth-commandcode", None, &desc, &bundle).unwrap();
        assert_eq!(key, "command-code-auth");
        let p = db.get_provider(&key).unwrap().unwrap();
        // 端点按预设建立，api_key 置空（凭据由 auth 插件出站注入）
        assert_eq!(
            p.endpoints[0].base_url,
            "https://api.commandcode.ai/provider/v1"
        );
        assert_eq!(p.endpoints[0].api_key, "");
        // extra.auth 只有非敏感元数据：绝不含令牌本体
        let extra_s = p.extra.to_string();
        assert!(
            !extra_s.contains("tok-1") && !extra_s.contains("ref-1"),
            "{extra_s}"
        );
        assert_eq!(p.extra["auth"]["plugin"], "auth-commandcode");
        assert_eq!(p.extra["auth"]["account"], "u1");
        // 令牌包在 secrets 表（密文，可解密回读）
        let raw = db
            .secret_get(&oauth::provider_scope(&key), oauth::BUNDLE_KEY)
            .unwrap()
            .unwrap();
        let back: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(back["access"], "tok-1");
        // provider 维度绑定已写
        assert!(db
            .plugin_enabled_in_scope("auth-commandcode", None, None, Some(&key))
            .unwrap());
        // 同类账户重新登录（provider 路径）：只刷元数据，created_at/端点保留
        let created = p.created_at;
        let bundle2 = json!({"access": "tok-2", "refresh": "ref-2", "source": "manual"});
        write_auth_provider(&db, "auth-commandcode", Some(&key), &desc, &bundle2).unwrap();
        let p2 = db.get_provider(&key).unwrap().unwrap();
        assert_eq!(p2.created_at, created);
        assert_eq!(
            p2.endpoints[0].base_url,
            "https://api.commandcode.ai/provider/v1"
        );
        let raw2 = db
            .secret_get(&oauth::provider_scope(&key), oauth::BUNDLE_KEY)
            .unwrap()
            .unwrap();
        assert!(raw2.contains("tok-2"));
        // 非法令牌包拒绝
        assert!(write_auth_provider(
            &db,
            "auth-commandcode",
            None,
            &desc,
            &json!({"refresh": "x"})
        )
        .is_err());
        // 插件路径缺 provider 模板拒绝（新建路径必需）
        assert!(write_auth_provider(
            &db,
            "auth-commandcode",
            None,
            &json!({"kind": "callback"}),
            &bundle
        )
        .is_err());
    }

    #[test]
    fn write_auth_provider_refuses_manual_conflict() {
        let db = Database::open_in_memory().unwrap();
        // 手建同名 provider（无 extra.auth）：不得被登录覆盖
        db.upsert_provider(&Provider {
            key: "kimi-oauth".into(),
            endpoints: vec![Endpoint {
                protocol: "openai-chat".into(),
                base_url: "https://x".into(),
                api_key: "manual".into(),
            }],
            version: None,
            user_agent: None,
            web_search: None,
            extra: Value::Null,
            enabled: true,
            quota_plugin_ref: String::new(),
            quota_interval_secs: 0,
            quota_enabled: false,
            quota_config: Value::Null,
            created_at: 0,
            updated_at: 0,
        })
        .unwrap();
        let bundle = json!({"access": "tok"});
        let desc = describe_tpl("kimi-oauth", "https://api.kimi.com/coding/v1");
        assert!(write_auth_provider(&db, "auth-kimi", None, &desc, &bundle).is_err());
        assert_eq!(
            db.get_provider("kimi-oauth").unwrap().unwrap().endpoints[0].api_key,
            "manual"
        );
    }

    /// begin 直出 done 的完整链路：内联假插件 + 真实编排函数 + 落库布局。
    #[tokio::test]
    async fn begin_done_full_chain() {
        let (st, dir) = temp_state("done");
        // 内联假认证插件（begin 直接返回 done）
        st.db
            .upsert_plugin(&moonbridge_store::PluginRecord {
                name: "auth-commandcode".into(),
                source: "lua".into(),
                script_ref: r#"
                    MB = { name = "auth-commandcode", capabilities = { "auth" } }
                    function MB.auth_describe(ctx)
                      return {
                        kind = "callback", supports_paste = true, instructions = "测试",
                        provider = {
                          key = "command-code-auth", label = "CC",
                          protocol = "openai-chat", base_url = "https://api.commandcode.ai/provider/v1",
                        },
                      }
                    end
                    function MB.auth_begin(ctx)
                      return { status = "done", bundle = { access = "tok-done", source = "local-cli" } }
                    end
                    function MB.auth_headers(ctx, b) return { { "authorization", "Bearer " .. b.access } } end
                "#.to_string(),
                enabled: false,
                config: json!({}),
                scopes: vec!["provider".into()],
                capabilities: vec!["auth".into()],
                category: "core".into(),
                config_schema: Value::Null,
            })
            .unwrap();
        let out = run_begin(&st, Some("auth-commandcode"), None, None)
            .await
            .unwrap();
        assert!(out.already_done);
        assert_eq!(out.provider_key.as_deref(), Some("command-code-auth"));
        assert!(out.supports_paste);
        assert_eq!(out.kind, "callback");
        // 落库布局与单测一致
        let p = st.db.get_provider("command-code-auth").unwrap().unwrap();
        assert_eq!(p.extra["auth"]["plugin"], "auth-commandcode");
        assert!(!p.extra.to_string().contains("tok-done"));
        let raw = st
            .db
            .secret_get(
                &oauth::provider_scope("command-code-auth"),
                oauth::BUNDLE_KEY,
            )
            .unwrap()
            .unwrap();
        assert!(raw.contains("tok-done"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// provider 路径：手建 provider + provider 维度绑定插件，登录只刷
    /// extra.auth 与令牌包，端点保持原样——第三方插件不依赖任何静态表。
    #[tokio::test]
    async fn begin_provider_path_updates_metadata_only() {
        let (st, dir) = temp_state("provpath");
        st.db
            .upsert_plugin(&moonbridge_store::PluginRecord {
                name: "auth-thirdparty".into(),
                source: "lua".into(),
                script_ref: r#"
                    MB = { name = "auth-thirdparty", capabilities = { "auth" } }
                    function MB.auth_describe(ctx)
                      return { kind = "callback" }
                    end
                    function MB.auth_begin(ctx)
                      return { status = "done", bundle = { access = "tok-x", account = "u9" } }
                    end
                    function MB.auth_headers(ctx, b) return { { "authorization", "Bearer " .. b.access } } end
                "#.to_string(),
                enabled: false,
                config: json!({}),
                scopes: vec!["provider".into()],
                capabilities: vec!["auth".into()],
                category: "auth".into(),
                config_schema: Value::Null,
            })
            .unwrap();
        // 手建 provider（用户自定义端点）+ 绑定 auth 插件
        st.db
            .upsert_provider(&Provider {
                key: "my-provider".into(),
                endpoints: vec![Endpoint {
                    protocol: "anthropic".into(),
                    base_url: "https://custom.example.com".into(),
                    api_key: String::new(),
                }],
                version: None,
                user_agent: None,
                web_search: None,
                extra: json!({}),
                enabled: true,
                quota_plugin_ref: String::new(),
                quota_interval_secs: 0,
                quota_enabled: false,
                quota_config: Value::Null,
                created_at: 0,
                updated_at: 0,
            })
            .unwrap();
        st.db
            .upsert_binding(&PluginBinding {
                plugin_name: "auth-thirdparty".into(),
                scope: "provider".into(),
                scope_key: "my-provider".into(),
                enabled: true,
                config: Value::Null,
            })
            .unwrap();
        let out = run_begin(&st, None, Some("my-provider"), None)
            .await
            .unwrap();
        assert!(out.already_done);
        assert_eq!(out.provider_key.as_deref(), Some("my-provider"));
        let p = st.db.get_provider("my-provider").unwrap().unwrap();
        // 端点/协议保持原样，extra.auth 更新，令牌包落 secrets
        assert_eq!(p.endpoints[0].protocol, "anthropic");
        assert_eq!(p.endpoints[0].base_url, "https://custom.example.com");
        assert_eq!(p.extra["auth"]["plugin"], "auth-thirdparty");
        assert_eq!(p.extra["auth"]["account"], "u9");
        let raw = st
            .db
            .secret_get(&oauth::provider_scope("my-provider"), oauth::BUNDLE_KEY)
            .unwrap()
            .unwrap();
        assert!(raw.contains("tok-x"));
        // 未绑定 auth 插件的 provider 拒绝进入流程
        let mut unbound = st.db.get_provider("my-provider").unwrap().unwrap();
        unbound.key = "unbound".into();
        unbound.created_at = 0;
        st.db.upsert_provider(&unbound).unwrap();
        assert!(run_begin(&st, None, Some("unbound"), None).await.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
