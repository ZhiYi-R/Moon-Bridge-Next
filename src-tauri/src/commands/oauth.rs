//! OAuth 登录编排 commands（**通用**：平台细节全部在 CAP_AUTH 插件内）。
//!
//! 形态：`oauth_describe` 返回插件自述（流程类型/来源选项/是否支持粘贴，供 UI
//! 决定渲染与来源选择）→ `oauth_begin` 启动后台登录任务并返回流程描述 → 前端以
//! `oauth_status` 轮询、`oauth_cancel` 中止、`oauth_paste` 提交手动粘贴。
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
use moonbridge_store::{Database, Endpoint, PluginBinding, Provider};
use serde::Serialize;
use serde_json::{json, Value};
use tauri::State;

use crate::commands::CmdResult;
use crate::presets::{self, ProviderPreset};
use crate::state::{ManagedState, OAuthFlow, OAuthFlowStatus};

/// `oauth_describe` 的返回：预设 id + 插件自述原样（kind/label/instructions/
/// supports_paste/sources 等由插件定义，UI 按元数据渲染，不含任何平台分支）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthDescribe {
    pub preset: String,
    pub describe: Value,
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

/// 账户预设解析：存在、启用、绑定了 CAP_AUTH 插件。
fn account_preset(preset_id: &str) -> CmdResult<&'static ProviderPreset> {
    let p = presets::PRESETS
        .iter()
        .find(|p| p.id == preset_id && p.category == "account" && p.enabled)
        .ok_or_else(|| format!("预设不存在或未启用: {preset_id}"))?;
    if p.auth_plugin.is_none() {
        return Err(format!("预设 {preset_id} 未绑定认证插件").into());
    }
    Ok(p)
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

/// 登录成功落库：令牌包 → secrets 表；provider 行（api_key 置空，extra.auth 记
/// 非敏感元数据）；provider 维度插件绑定。同名冲突只允许覆盖同类 OAuth 账户。
fn write_auth_provider(
    db: &Database,
    preset: &ProviderPreset,
    bundle: &Value,
) -> Result<String, String> {
    oauth::validate_bundle(bundle)?;
    let plugin = preset
        .auth_plugin
        .ok_or_else(|| format!("预设 {} 未绑定认证插件", preset.id))?;
    let key = preset.id.to_string();
    let mut created_at = 0;
    if let Some(old) = db.get_provider(&key).map_err(|e| e.to_string())? {
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
        created_at = old.created_at;
    }
    // 令牌包加密存 secrets 表（不进 provider.extra、不随 provider_get 回传前端）
    oauth::write_bundle(db, &key, bundle)?;
    let meta = json!({
        "plugin": plugin,
        "account": bundle.get("account"),
        "email": bundle.get("email"),
        "source": bundle.get("source"),
    });
    let p = Provider {
        key: key.clone(),
        endpoints: vec![Endpoint {
            protocol: preset.protocol.to_string(),
            base_url: preset.base_url.to_string(),
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

/// 插件自述（UI 先调它决定渲染与来源选择，再调 `oauth_begin`）。
#[tauri::command]
pub async fn oauth_describe(
    state: State<'_, Arc<ManagedState>>,
    preset: String,
) -> CmdResult<OAuthDescribe> {
    let st = state.inner().clone();
    let p = account_preset(&preset)?;
    let (rt, _callbacks) = flow_runtime(&st, p.auth_plugin.unwrap())?;
    let describe = rt
        .call_mb_once("auth_describe", &oauth::auth_ctx(p.id))
        .await
        .map_err(|e| format!("auth_describe 调用失败: {e}"))?;
    Ok(OAuthDescribe {
        preset: p.id.to_string(),
        describe,
    })
}

/// 启动登录流程（`source` 为 UI 传来的来源选择；describe.sources 未提供时传 None）。
#[tauri::command]
pub async fn oauth_begin(
    state: State<'_, Arc<ManagedState>>,
    preset: String,
    source: Option<String>,
) -> CmdResult<OAuthBegin> {
    let st = state.inner().clone();
    run_begin(&st, &preset, source).await
}

pub async fn run_begin(
    st: &Arc<ManagedState>,
    preset_id: &str,
    source: Option<String>,
) -> CmdResult<OAuthBegin> {
    let p = account_preset(preset_id)?;
    let plugin = p.auth_plugin.unwrap();
    let (rt, callbacks) = flow_runtime(st, plugin)?;
    let describe = rt
        .call_mb_once("auth_describe", &oauth::auth_ctx(p.id))
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

    let ctx = oauth::auth_ctx_with(p.id, source.as_deref());
    let begin = rt
        .call_mb_once("auth_begin", &ctx)
        .await
        .map_err(|e| format!("auth_begin 调用失败: {e}"))?;
    // begin 直出终态：done（本地导入命中）/ error（显式失败，如 local-cli 验证失败）
    match begin.get("status").and_then(Value::as_str) {
        Some("done") => {
            let key = write_auth_provider(&st.db, p, &begin["bundle"])?;
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
    let preset: &'static ProviderPreset = p;
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
                                "done" => break write_auth_provider(&db, preset, &v["bundle"]),
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

    fn preset_of(id: &str) -> &'static ProviderPreset {
        presets::PRESETS.iter().find(|p| p.id == id).unwrap()
    }

    #[test]
    fn write_auth_provider_layout() {
        let db = Database::open_in_memory().unwrap();
        let bundle = json!({
            "access": "tok-1", "refresh": "ref-1", "expires_at": 123,
            "account": "u1", "email": "a@b.com", "source": "oauth",
        });
        let key = write_auth_provider(&db, preset_of("command-code-auth"), &bundle).unwrap();
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
        // 同类账户重新登录：原位更新，created_at 保留
        let created = p.created_at;
        let bundle2 = json!({"access": "tok-2", "refresh": "ref-2", "source": "manual"});
        write_auth_provider(&db, preset_of("command-code-auth"), &bundle2).unwrap();
        let p2 = db.get_provider(&key).unwrap().unwrap();
        assert_eq!(p2.created_at, created);
        let raw2 = db
            .secret_get(&oauth::provider_scope(&key), oauth::BUNDLE_KEY)
            .unwrap()
            .unwrap();
        assert!(raw2.contains("tok-2"));
        // 非法令牌包拒绝
        assert!(write_auth_provider(
            &db,
            preset_of("command-code-auth"),
            &json!({"refresh": "x"})
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
        assert!(write_auth_provider(&db, preset_of("kimi-oauth"), &bundle).is_err());
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
                      return { kind = "callback", supports_paste = true, instructions = "测试" }
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
        let out = run_begin(&st, "command-code-auth", None).await.unwrap();
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
}
