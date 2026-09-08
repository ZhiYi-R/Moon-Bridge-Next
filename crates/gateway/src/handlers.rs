//! axum HTTP handlers：各协议入口端点、健康检查、模型列表。

use std::sync::Arc;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Response;
use axum::Json;
use moonbridge_core::Protocol;
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

/// GET /v1/models —— 列出可用模型（routes 别名 + provider 限定名）。
pub async fn models(State(state): State<Arc<AppState>>) -> Result<Json<Value>> {
    let mut data = Vec::new();
    for r in state.db.list_routes()? {
        data.push(json!({
            "id": r.alias,
            "object": "model",
            "owned_by": r.provider_key,
        }));
    }
    for p in state.db.list_providers()? {
        for o in state.db.list_offers(&p.key)? {
            data.push(json!({
                "id": format!("{}({})", o.model_slug, p.key),
                "object": "model",
                "owned_by": p.key,
            }));
        }
    }
    Ok(Json(json!({ "object": "list", "data": data })))
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
