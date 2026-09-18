
//! OAuth 登录编排 commands（Kimi 设备码 / Command Code 回调 + 本地导入 + 粘贴）。
//!
//! 形态：`oauth_begin` 启动后台任务并立即返回流程描述；前端以 `oauth_status` 轮询、
//! `oauth_cancel` 中止、`oauth_paste` 提交手动粘贴（Command Code 兜底）。登录成功的
//! 令牌落库为 provider（key = 预设 id），令牌包在 `extra_json.oauth`——网关侧的
//! 刷新链路见 `moonbridge_gateway::oauth::ensure_fresh`。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use moonbridge_gateway::oauth;
use moonbridge_store::{Database, Endpoint, Provider};
use serde::Serialize;
use serde_json::{Map, Value};
use tauri::State;

use crate::commands::CmdResult;
use crate::config::AppPaths;
use crate::presets;
use crate::state::{ManagedState, OAuthFlow, OAuthFlowStatus};

/// `oauth_begin` 的返回。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthBegin {
    pub flow_id: String,
    /// device（Kimi 设备码）/ callback（Command Code 浏览器回调 + 粘贴兜底）。
    pub kind: String,
    /// begin 阶段已完成（本地 CLI 凭据导入命中，无后台任务）。
    pub already_done: bool,
    /// 需要用户打开的页面（设备验证页 / Studio 授权页）。
    pub verification_url: Option<String>,
    /// 设备码（kind = device 时展示给用户输入）。
    pub user_code: Option<String>,
    pub instructions: Option<String>,
    /// already_done 时创建/更新的 provider key。
    pub provider_key: Option<String>,
}

/// 登录用 HTTP 客户端（与 catalog 拉取同口径：无 egress 代理，30s 超时）。
fn oauth_http_client() -> CmdResult<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| format!("构建 HTTP 客户端失败: {e}").into())
}

/// Kimi 设备 id：读 `data_dir/kimi-device-id`，不存在则生成并落盘（安装级稳定）。
fn kimi_device_id(paths: &AppPaths) -> std::io::Result<String> {
    let path = paths.data_dir.join("kimi-device-id");
    if let Ok(s) = std::fs::read_to_string(&path) {
        let s = s.trim().to_string();
        if !s.is_empty() {
            return Ok(s);
        }
    }
    let id = uuid::Uuid::new_v4().simple().to_string();
    std::fs::write(&path, format!("{id}\n"))?;
    Ok(id)
}

/// 登录成功落库为 provider：key = 预设 id；同名冲突只允许覆盖同类 OAuth 账户
/// （重新登录场景），不得覆盖用户手建的同名 provider。
fn write_oauth_provider(
    db: &Database,
    preset_id: &str,
    bundle: &oauth::OAuthBundle,
) -> Result<String, String> {
    let preset = presets::PRESETS
        .iter()
        .find(|p| p.id == preset_id && p.category == "account" && p.enabled)
        .ok_or_else(|| format!("预设不存在或未启用: {preset_id}"))?;
    let key = preset.id.to_string();
    let mut created_at = 0;
    if let Some(old) = db.get_provider(&key).map_err(|e| e.to_string())? {
        let old_kind = oauth::bundle_of(&old).map(|b| b.kind);
        if old_kind.as_deref() != Some(bundle.kind.as_str()) {
            return Err(format!("上游 “{key}” 已存在且不是同名 OAuth 账户，请先删除"));
        }
        created_at = old.created_at;
    }
    let mut extra = Map::new();
    extra.insert(
        "oauth".to_string(),
        serde_json::to_value(bundle).map_err(|e| e.to_string())?,
    );
    let p = Provider {
        key: key.clone(),
        endpoints: vec![Endpoint {
            protocol: preset.protocol.to_string(),
            base_url: preset.base_url.to_string(),
            api_key: bundle.access.clone(),
        }],
        version: None,
        user_agent: None,
        web_search: None,
        extra: Value::Object(extra),
        enabled: true,
        created_at,
        updated_at: 0,
    };
    db.upsert_provider(&p).map_err(|e| e.to_string())?;
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

/// 粘贴无效时的提示（流程保持 pending，可继续粘贴或等回调）。
fn set_pending_msg(status: &Mutex<OAuthFlowStatus>, msg: String) {
    let mut g = status.lock().unwrap();
    g.state = "pending".to_string();
    g.message = Some(msg);
}

/// 启动 OAuth 登录流程。
#[tauri::command]
pub async fn oauth_begin(state: State<'_, Arc<ManagedState>>, preset: String) -> CmdResult<OAuthBegin> {
    let st = state.inner().clone();
    match preset.as_str() {
        "kimi-oauth" => begin_kimi(st).await,
        "command-code-auth" => begin_command_code(st).await,
        other => Err(format!("不支持的 OAuth 预设: {other}").into()),
    }
}

async fn begin_kimi(state: Arc<ManagedState>) -> CmdResult<OAuthBegin> {
    let device_id =
        kimi_device_id(&state.paths).map_err(|e| format!("设备 id 读写失败: {e}"))?;
    let client = oauth_http_client()?;
    let auth = oauth::kimi_device_authorize(&client, &device_id).await?;

    let flow_id = uuid::Uuid::new_v4().simple().to_string();
    // 给前端的展示字段先拷出，auth 本体移入后台轮询任务
    let verification_url = auth.verification_url.clone();
    let user_code = auth.user_code.clone();
    let (abort_tx, mut abort_rx) = tokio::sync::oneshot::channel::<()>();
    let (paste_tx, _paste_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let status = Arc::new(Mutex::new(OAuthFlowStatus::pending()));
    let status2 = status.clone();
    let db = state.db.clone();
    tokio::spawn(async move {
        let result = match oauth::kimi_poll_token(&client, &device_id, &auth, &mut abort_rx).await {
            Ok(bundle) => write_oauth_provider(&db, "kimi-oauth", &bundle),
            Err(e) => Err(e),
        };
        finish_flow(result, &status2);
    });
    state.oauth_flows.lock().unwrap().insert(
        flow_id.clone(),
        OAuthFlow { status, abort: abort_tx, paste: paste_tx },
    );
    Ok(OAuthBegin {
        flow_id,
        kind: "device".to_string(),
        already_done: false,
        verification_url: Some(verification_url),
        user_code: Some(user_code),
        instructions: Some("打开验证页，输入验证码完成授权".to_string()),
        provider_key: None,
    })
}

async fn begin_command_code(state: Arc<ManagedState>) -> CmdResult<OAuthBegin> {
    let client = oauth_http_client()?;
    // 本地 CLI 凭据导入优先（已登录过 CLI 则无需浏览器）
    if let Some(bundle) = oauth::command_code_import_local(&client).await {
        let key = write_oauth_provider(&state.db, "command-code-auth", &bundle)?;
        return Ok(OAuthBegin {
            flow_id: String::new(),
            kind: "callback".to_string(),
            already_done: true,
            verification_url: None,
            user_code: None,
            instructions: Some("已导入本地 CLI 凭据".to_string()),
            provider_key: Some(key),
        });
    }

    let listener = oauth::start_command_code_listener().await?;
    let url = oauth::command_code_auth_url(listener.port, &listener.state);
    let flow_id = uuid::Uuid::new_v4().simple().to_string();
    let (abort_tx, mut abort_rx) = tokio::sync::oneshot::channel::<()>();
    let (paste_tx, mut paste_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let status = Arc::new(Mutex::new(OAuthFlowStatus::pending()));
    let status2 = status.clone();
    let db = state.db.clone();
    let expected_state = listener.state.clone();
    tokio::spawn(async move {
        let mut listener = listener;
        let timeout = tokio::time::sleep(Duration::from_secs(120));
        tokio::pin!(timeout);
        let result: Result<String, String> = loop {
            tokio::select! {
                cb = &mut listener.rx => {
                    break match cb {
                        Ok(cb) => {
                            let bundle = oauth::OAuthBundle::durable(
                                "command-code", cb.api_key,
                                Some(cb.user_id).filter(|s| !s.is_empty()), "oauth",
                            );
                            write_oauth_provider(&db, "command-code-auth", &bundle)
                        }
                        Err(_) => Err("回调通道已关闭".to_string()),
                    };
                }
                pasted = paste_rx.recv() => {
                    // 粘贴通道关闭（cancel 移除流程）时该分支返回 None：中止退出
                    let Some(text) = pasted else { break Err("登录已取消".to_string()); };
                    match oauth::parse_command_code_paste(&text, &expected_state) {
                        Err(e) => {
                            set_pending_msg(&status2, e);
                            continue;
                        }
                        Ok(oauth::CommandCodePaste::RawKey(key)) => {
                            match oauth::command_code_whoami(&client, &key).await {
                                Some((id, _)) => {
                                    break write_oauth_provider(
                                        &db, "command-code-auth",
                                        &oauth::OAuthBundle::durable("command-code", key, Some(id), "manual"),
                                    );
                                }
                                None => {
                                    set_pending_msg(&status2, "API Key 验证失败，请检查后重试".to_string());
                                    continue;
                                }
                            }
                        }
                        Ok(oauth::CommandCodePaste::Callback(cb)) => {
                            let bundle = oauth::OAuthBundle::durable(
                                "command-code", cb.api_key,
                                Some(cb.user_id).filter(|s| !s.is_empty()), "manual",
                            );
                            break write_oauth_provider(&db, "command-code-auth", &bundle);
                        }
                    }
                }
                () = &mut timeout => break Err("登录超时（120 秒），请重新发起".to_string()),
                _ = &mut abort_rx => break Err("登录已取消".to_string()),
            }
        };
        finish_flow(result, &status2);
        // listener 随此作用域释放：shutdown sender Drop 触发优雅关闭
        drop(listener);
    });
    state.oauth_flows.lock().unwrap().insert(
        flow_id.clone(),
        OAuthFlow { status, abort: abort_tx, paste: paste_tx },
    );
    Ok(OAuthBegin {
        flow_id,
        kind: "callback".to_string(),
        already_done: false,
        verification_url: Some(url),
        user_code: None,
        instructions: Some("浏览器完成授权后自动回跳；也可手动粘贴回调信息或 API Key".to_string()),
        provider_key: None,
    })
}

/// 查询登录流程状态。
#[tauri::command]
pub fn oauth_status(state: State<'_, Arc<ManagedState>>, flow_id: String) -> CmdResult<OAuthFlowStatus> {
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
    // 显式 drop 两个发送端：后台任务的 abort/paste 通道读到关闭即退出
    if let Some(flow) = state.oauth_flows.lock().unwrap().remove(&flow_id) {
        drop(flow.abort);
        drop(flow.paste);
    }
    Ok(())
}

/// 提交手动粘贴（Command Code 兜底：回调 JSON / 回调 URL / 裸 API Key）。
#[tauri::command]
pub fn oauth_paste(state: State<'_, Arc<ManagedState>>, flow_id: String, text: String) -> CmdResult<()> {
    let flows = state.oauth_flows.lock().unwrap();
    let f = flows
        .get(&flow_id)
        .ok_or_else(|| format!("登录流程不存在或已结束: {flow_id}"))?;
    f.paste.send(text).map_err(|_| "登录流程已结束".to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_oauth_provider_creates_and_relogins() {
        let db = Database::open_in_memory().unwrap();
        let bundle = oauth::OAuthBundle::durable("command-code", "key-1".into(), Some("u1".into()), "oauth");
        let key = write_oauth_provider(&db, "command-code-auth", &bundle).unwrap();
        assert_eq!(key, "command-code-auth");
        let p = db.get_provider(&key).unwrap().unwrap();
        assert_eq!(p.endpoints[0].api_key, "key-1");
        assert_eq!(p.endpoints[0].base_url, "https://api.commandcode.ai/provider/v1");
        assert_eq!(p.endpoints[0].protocol, "openai-chat");
        assert_eq!(oauth::bundle_of(&p).unwrap().kind, "command-code");

        // 同类 OAuth 账户重新登录：原位更新令牌，created_at 保留
        let created = p.created_at;
        let bundle2 = oauth::OAuthBundle::durable("command-code", "key-2".into(), Some("u1".into()), "manual");
        write_oauth_provider(&db, "command-code-auth", &bundle2).unwrap();
        let p2 = db.get_provider(&key).unwrap().unwrap();
        assert_eq!(p2.endpoints[0].api_key, "key-2");
        assert_eq!(p2.created_at, created);
    }

    #[test]
    fn write_oauth_provider_refuses_non_oauth_conflict() {
        let db = Database::open_in_memory().unwrap();
        // 手建同名 provider（无 oauth 包）：不得被登录覆盖
        db.upsert_provider(&Provider {
            key: "command-code-auth".into(),
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
            created_at: 0,
            updated_at: 0,
        })
        .unwrap();
        let bundle = oauth::OAuthBundle::durable("command-code", "key-1".into(), None, "oauth");
        assert!(write_oauth_provider(&db, "command-code-auth", &bundle).is_err());
        assert_eq!(
            db.get_provider("command-code-auth").unwrap().unwrap().endpoints[0].api_key,
            "manual"
        );
    }

    #[test]
    fn kimi_device_id_persists() {
        let dir = std::env::temp_dir().join(format!("mb-oauth-{}", std::process::id()));
        let paths = AppPaths::resolve(dir.join("config"), dir.join("data"));
        paths.ensure_dirs().unwrap();
        let a = kimi_device_id(&paths).unwrap();
        let b = kimi_device_id(&paths).unwrap();
        assert_eq!(a, b, "设备 id 应安装级稳定");
        assert!(!a.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
