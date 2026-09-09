//! axum HTTP handlers：各协议入口端点、健康检查、模型列表。

use std::collections::HashSet;
use std::sync::Arc;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Response;
use axum::Json;
use moonbridge_core::Protocol;
use moonbridge_store::Database;
use serde_json::{json, Value};

use crate::dispatch;
use crate::error::{GatewayError, Result};
use crate::state::AppState;

/// POST /v1/responses —— OpenAI Responses 入口。
pub async fn responses(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Response> {
    handle(state, Protocol::OpenAiResponse, headers, body).await
}

/// POST /v1/messages —— Anthropic Messages 入口（M6 完整支持，此处先接入）。
pub async fn messages(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Response> {
    handle(state, Protocol::Anthropic, headers, body).await
}

/// POST /v1/chat/completions —— OpenAI Chat 入口（M6 完整支持）。
pub async fn chat_completions(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Response> {
    handle(state, Protocol::OpenAiChat, headers, body).await
}

/// GET /health —— 健康检查。
pub async fn health() -> &'static str {
    "ok"
}

/// GET /v1/models —— 列出可用模型。
///
/// 聚合口径与 [`crate::router::Router::resolve`] 的解析优先级严格一致，只列客户端
/// **实际可路由**的名字：
/// 1. routes 别名；
/// 2. offers 裸模型名（任一**启用** provider 提供即可直接用作 model 参数；
///    与别名或更早裸名冲突时跳过——解析时别名优先，裸名会被截获不可达）；
/// 3. `model(provider)` 限定名（显式指定 provider，不受冲突与启用状态影响）。
pub async fn models(State(state): State<Arc<AppState>>) -> Result<Json<Value>> {
    Ok(Json(models_payload(&state.db)?))
}

fn models_payload(db: &Database) -> Result<Value> {
    let mut data = Vec::new();
    let mut taken: HashSet<String> = HashSet::new();
    for r in db.list_routes()? {
        if taken.insert(r.alias.clone()) {
            data.push(json!({
                "id": r.alias,
                "object": "model",
                "owned_by": r.provider_key,
            }));
        }
    }
    for p in db.list_providers()? {
        for o in db.list_offers(&p.key)? {
            // 限定名恒可达（Router 路径 1 不检查 provider 启用状态）
            data.push(json!({
                "id": format!("{}({})", o.model_slug, p.key),
                "object": "model",
                "owned_by": p.key,
            }));
            // 裸名：仅启用 provider 可达（Router 路径 3），且不得与别名/更早裸名冲突
            if p.enabled && taken.insert(o.model_slug.clone()) {
                data.push(json!({
                    "id": o.model_slug,
                    "object": "model",
                    "owned_by": p.key,
                }));
            }
        }
    }
    Ok(json!({ "object": "list", "data": data }))
}

/// 入口公共处理：认证 → 提取头/会话 → dispatch。
async fn handle(
    state: Arc<AppState>,
    protocol: Protocol,
    headers: HeaderMap,
    body: Value,
) -> Result<Response> {
    check_auth(&state, &headers)?;
    let req_headers = extract_headers(&headers);
    let session_id = extract_session(&headers, &body);
    dispatch::handle_request(state, protocol, body, req_headers, session_id).await
}

/// Bearer 认证（未配置 auth_token 时放行）。
fn check_auth(state: &AppState, headers: &HeaderMap) -> Result<()> {
    let Some(token) = &state.config.auth_token else {
        return Ok(());
    };
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let provided = auth.strip_prefix("Bearer ").unwrap_or(auth);
    if provided != token {
        return Err(GatewayError::Auth("无效或缺失的 Bearer token".to_string()));
    }
    Ok(())
}

/// 提取全部请求头（保序，供报文层钩子读写）。
fn extract_headers(headers: &HeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
        .collect()
}

/// 解析会话 ID：body.session_id > previous_response_id > X-Codex-Window-Id。
fn extract_session(headers: &HeaderMap, body: &Value) -> Option<String> {
    body.get("session_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            body.get("previous_response_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .or_else(|| {
            headers
                .get("x-codex-window-id")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use moonbridge_store::{Endpoint, Offer, Provider, Route};

    fn provider(key: &str, enabled: bool) -> Provider {
        Provider {
            key: key.into(),
            endpoints: vec![Endpoint {
                protocol: "anthropic".into(),
                base_url: "https://x".into(),
                api_key: "k".into(),
            }],
            version: None,
            user_agent: None,
            web_search: None,
            extra: Value::Null,
            enabled,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn offer(provider_key: &str, slug: &str) -> Offer {
        Offer {
            provider_key: provider_key.into(),
            model_slug: slug.into(),
            pricing: None,
            endpoint_protocol: None,
        }
    }

    fn ids(v: &Value) -> Vec<&str> {
        v["data"].as_array().unwrap().iter().map(|m| m["id"].as_str().unwrap()).collect()
    }

    /// 裸名/别名/限定名三类都应聚合；多 provider 提供同名时裸名去重。
    #[test]
    fn aggregates_routes_and_offer_names() {
        let db = Database::open_in_memory().unwrap();
        db.upsert_provider(&provider("zen", true)).unwrap();
        db.upsert_provider(&provider("deepseek", true)).unwrap();
        db.upsert_offer(&offer("zen", "muse")).unwrap();
        db.upsert_offer(&offer("deepseek", "muse")).unwrap();
        db.upsert_offer(&offer("deepseek", "spark")).unwrap();
        db.upsert_route(&Route {
            alias: "fast".into(),
            model_slug: "muse".into(),
            provider_key: "zen".into(),
            extra: Value::Null,
        })
        .unwrap();

        let out = models_payload(&db).unwrap();
        let got = ids(&out);
        assert!(got.contains(&"fast"), "路由别名: {got:?}");
        assert_eq!(got.iter().filter(|i| **i == "muse").count(), 1, "裸名去重");
        assert!(got.contains(&"spark"), "裸名");
        assert!(got.contains(&"muse(zen)"), "限定名");
        assert!(got.contains(&"muse(deepseek)"), "限定名");
    }

    /// 别名优先于裸名：与别名同名的裸名不可达，不应列出；停用 provider
    /// 的裸名不可达（不列），但限定名仍可达（列出）。
    #[test]
    fn respects_route_precedence_and_enabled_state() {
        let db = Database::open_in_memory().unwrap();
        db.upsert_provider(&provider("zen", true)).unwrap();
        db.upsert_provider(&provider("off", false)).unwrap();
        db.upsert_offer(&offer("zen", "fast")).unwrap(); // 与别名冲突的裸名
        db.upsert_offer(&offer("off", "paused")).unwrap();
        db.upsert_route(&Route {
            alias: "fast".into(),
            model_slug: "muse".into(),
            provider_key: "zen".into(),
            extra: Value::Null,
        })
        .unwrap();

        let out = models_payload(&db).unwrap();
        let got = ids(&out);
        assert_eq!(got.iter().filter(|i| **i == "fast").count(), 1, "别名占位，裸名跳过");
        assert!(!got.contains(&"paused"), "停用 provider 裸名不可达");
        assert!(got.contains(&"paused(off)"), "停用 provider 限定名仍可达");
    }
}
