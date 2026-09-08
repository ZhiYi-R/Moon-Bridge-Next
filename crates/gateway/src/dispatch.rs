//! 请求生命周期编排（dispatch）。
//!
//! 严格按计划的数据流串联 RAW/CORE 两组钩子：
//! ```text
//! [RAW] on_client_request_raw → to_core_request → 路由
//!   → [CORE] on_request/inject_tools → from_core_request
//!   → [RAW] on_upstream_request_raw → 发送上游
//!   → 流式: read_chunks → [RAW] chunk → decode → [CORE] event → encode → [RAW] chunk → 写出
//!     非流式: [RAW] on_upstream_response_raw → to_core_response → [CORE] on_response
//!             → from_core_response → [RAW] on_client_response_raw → 回写
//! ```

use std::sync::Arc;
use std::time::Instant;

use axum::body::Body;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use moonbridge_core::{Protocol, Usage};
use moonbridge_protocol::{
    RawBody, RawMessage, RawStage, RawVerdict, ReqCtx, UpstreamRequest,
};
use serde_json::Value;

use crate::error::{GatewayError, Result};
use crate::router::Router;
use crate::state::AppState;
use crate::stream;
use crate::trace::{self, TraceRecord};
use crate::upstream;
use crate::usage;

/// 处理一次入口请求，返回 axum 响应（流式 SSE 或非流式 JSON）。
pub async fn handle_request(
    state: Arc<AppState>,
    client_protocol: Protocol,
    raw_body: Value,
    req_headers: Vec<(String, String)>,
    session_id: Option<String>,
) -> Result<Response> {
    let start = Instant::now();
    let request_id = uuid::Uuid::new_v4().to_string();
    let model_alias = raw_body
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    let mut ctx = ReqCtx::new(request_id, client_protocol);
    ctx.session_id = session_id;
    ctx.model_alias = model_alias;

    // ── [RAW] 入站请求钩子 ──
    let mut inbound = RawMessage {
        stage: RawStage::ClientRequest,
        protocol: client_protocol,
        provider: None,
        method: Some("POST".to_string()),
        url: None,
        status: None,
        headers: req_headers,
        body: RawBody::json(raw_body),
    };
    match state.hooks.on_client_request_raw(&ctx, &mut inbound).await? {
        RawVerdict::ShortCircuit { status, headers, body } => {
            return Ok(short_circuit(status, headers, body));
        }
        RawVerdict::Abort { message } => return Err(GatewayError::Other(message)),
        RawVerdict::Pass => {}
    }
    let raw_body = take_json_body(inbound.body, client_protocol)?;
    // 留存入站请求快照供 trace 落盘（raw_body 随后被 to_core_request 消费）
    let client_request_snapshot = raw_body.clone();

    // ── 入口协议 → Core ──
    let client_adapter = state
        .registry
        .client(client_protocol)
        .ok_or_else(|| GatewayError::Route(format!("无入口 Adapter 支持协议 {client_protocol}")))?;
    let mut core_req = client_adapter.to_core_request(&ctx, raw_body).await?;
    ctx.stream = core_req.stream;

    // ── 路由解析 ──
    let resolved = Router::resolve(&state.db, &core_req.model_alias)?;
    core_req.model = resolved.upstream_model.clone();
    ctx = ctx.with_route(resolved.protocol, resolved.provider_key.clone());

    // ── [CORE] 请求钩子 + 工具注入 ──
    state.hooks.on_request(&ctx, &mut core_req).await?;
    let extra_tools = state.hooks.inject_tools(&ctx).await?;
    core_req.tools.extend(extra_tools);

    // ── Core → 上游协议（逐端点故障转移）──

    // 故障转移：按序尝试各端点，连接错误/超时、429、5xx 且还有后续端点时切换；
    // 其余 4xx 或末位端点失败则快速失败。协议绑定在端点上，Adapter 逐端点选取；
    // outbound 钩子随端点逐次触发。
    let total = resolved.endpoints.len();
    let mut up: Option<UpstreamRequest> = None;
    let mut resp: Option<reqwest::Response> = None;
    let mut used_protocol = resolved.protocol;
    for (attempt, ep) in resolved.endpoints.iter().enumerate() {
        let provider_adapter = state.registry.provider(ep.protocol).ok_or_else(|| {
            GatewayError::Route(format!("无上游 Adapter 支持协议 {}", ep.protocol))
        })?;
        let mut u = provider_adapter
            .from_core_request(&ctx, &core_req, ep)
            .await?;

        // ── [RAW] 出站请求钩子（每端点一次）──
        let mut outbound = RawMessage {
            stage: RawStage::UpstreamRequest,
            protocol: ep.protocol,
            provider: Some(resolved.provider_key.clone()),
            method: Some(u.method.to_string()),
            url: Some(u.url.clone()),
            status: None,
            headers: u.headers.clone(),
            body: RawBody::json(u.body.clone()),
        };
        match state
            .hooks
            .on_upstream_request_raw(&ctx, &mut outbound)
            .await?
        {
            RawVerdict::ShortCircuit { status, headers, body } => {
                return Ok(short_circuit(status, headers, body));
            }
            RawVerdict::Abort { message } => return Err(GatewayError::Other(message)),
            RawVerdict::Pass => {}
        }
        if let Some(url) = outbound.url {
            u.url = url;
        }
        u.headers = outbound.headers;
        if let RawBody::Json { value } = outbound.body {
            u.body = value;
        }

        match upstream::send(&state.client, &u).await {
            Ok(r) => {
                let status = r.status();
                let retryable = status.as_u16() == 429 || status.is_server_error();
                if retryable && attempt + 1 < total {
                    let _ = r.text().await.unwrap_or_default();
                    tracing::warn!(
                        provider = %resolved.provider_key,
                        endpoint = %ep.base_url,
                        status = status.as_u16(),
                        "端点失败，故障转移到下一端点"
                    );
                    continue;
                }
                up = Some(u);
                resp = Some(r);
                used_protocol = ep.protocol;
                break;
            }
            Err(e) => {
                if attempt + 1 < total {
                    tracing::warn!(
                        provider = %resolved.provider_key,
                        endpoint = %ep.base_url,
                        error = %e,
                        "端点请求失败，故障转移到下一端点"
                    );
                    continue;
                }
                return Err(e);
            }
        }
    }
    // 路由器保证 endpoints 非空，循环必以成功或提前 return 结束
    let up = up.expect("endpoints 非空");
    let resp = resp.expect("endpoints 非空");

    // 协议绑定在端点上：以实际命中的端点协议覆写 ctx（流式回程按它选流式 Adapter）
    ctx = ctx.with_route(used_protocol, resolved.provider_key.clone());

    let status = resp.status();

    // ── trace 骨架（请求侧；响应侧回程时补齐）──
    let mut trace = TraceRecord {
        request_id: ctx.request_id.clone(),
        created_at: trace::now_ms(),
        session_id: ctx.session_id.clone(),
        model_alias: ctx.model_alias.clone(),
        upstream_model: resolved.upstream_model.clone(),
        provider_key: resolved.provider_key.clone(),
        client_protocol: ctx.client_protocol.to_string(),
        upstream_protocol: used_protocol.to_string(),
        stream: core_req.stream,
        status: "ok".to_string(),
        latency_ms: 0,
        usage: Usage::default(),
        client_request: client_request_snapshot,
        upstream_request: serde_json::json!({
            "method": up.method.to_string(),
            "url": up.url,
            "headers": up.headers,
            "body": up.body,
        }),
        upstream_response: Value::Null,
        client_response: Value::Null,
        error: None,
    };

    // ── 发送上游 ──（已在故障转移循环内完成）
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        let msg = state
            .hooks
            .transform_error(&ctx, &text)
            .await
            .unwrap_or_else(|_| text.clone());
        usage::record(
            &state.db,
            &ctx,
            &resolved.upstream_model,
            &Usage::default(),
            "error",
            Some(&msg),
            start,
            None,
        );
        trace.status = "error".to_string();
        trace.error = Some(msg.clone());
        trace.latency_ms = start.elapsed().as_millis() as u64;
        trace::write(state.config.trace_dir.as_deref(), &trace);
        return Err(GatewayError::Upstream {
            status: status.as_u16(),
            message: msg,
        });
    }

    // ── 流式 / 非流式分流 ──
    if core_req.stream {
        Ok(stream::build_stream_response(
            state,
            ctx,
            resp,
            resolved.upstream_model,
            start,
            trace,
        ))
    } else {
        non_stream(state, ctx, resp, used_protocol, start, trace).await
    }
}

/// 非流式回程编排。`upstream_protocol` 为实际命中端点的协议。
async fn non_stream(
    state: Arc<AppState>,
    ctx: ReqCtx,
    resp: reqwest::Response,
    upstream_protocol: Protocol,
    start: Instant,
    mut trace: TraceRecord,
) -> Result<Response> {
    let provider_adapter = state
        .registry
        .provider(upstream_protocol)
        .ok_or_else(|| {
            GatewayError::Route(format!("无上游 Adapter 支持协议 {upstream_protocol}"))
        })?;
    let client_adapter = state
        .registry
        .client(ctx.client_protocol)
        .ok_or_else(|| {
            GatewayError::Route(format!("无入口 Adapter 支持协议 {}", ctx.client_protocol))
        })?;

    let status = resp.status().as_u16();
    let resp_headers: Vec<(String, String)> = resp
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();
    let bytes = resp.bytes().await?;
    let body_value: Value = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).to_string()));

    // ── [RAW] 入站响应钩子 ──
    let mut inbound_resp = RawMessage {
        stage: RawStage::UpstreamResponse,
        protocol: upstream_protocol,
        provider: ctx.provider_key.clone(),
        method: None,
        url: None,
        status: Some(status),
        headers: resp_headers,
        body: RawBody::json(body_value),
    };
    match state
        .hooks
        .on_upstream_response_raw(&ctx, &mut inbound_resp)
        .await?
    {
        RawVerdict::ShortCircuit { status, headers, body } => {
            return Ok(short_circuit(status, headers, body));
        }
        RawVerdict::Abort { message } => return Err(GatewayError::Other(message)),
        RawVerdict::Pass => {}
    }
    let body_value = take_json_body(inbound_resp.body, upstream_protocol)?;
    trace.upstream_response = body_value.clone();

    // ── 上游 → Core ──
    let mut core_resp = provider_adapter.to_core_response(&ctx, body_value).await?;
    // ── [CORE] 响应钩子 ──
    state.hooks.on_response(&ctx, &mut core_resp).await?;
    let usage_snapshot = core_resp.usage;

    // ── Core → 入口协议 ──
    let client_json = client_adapter.from_core_response(&ctx, core_resp).await?;

    // ── [RAW] 出站响应钩子 ──
    let mut outbound_resp = RawMessage {
        stage: RawStage::ClientResponse,
        protocol: ctx.client_protocol,
        provider: None,
        method: None,
        url: None,
        status: Some(200),
        headers: vec![("content-type".to_string(), "application/json".to_string())],
        body: RawBody::json(client_json),
    };
    match state
        .hooks
        .on_client_response_raw(&ctx, &mut outbound_resp)
        .await?
    {
        RawVerdict::ShortCircuit { status, headers, body } => {
            return Ok(short_circuit(status, headers, body));
        }
        RawVerdict::Abort { message } => return Err(GatewayError::Other(message)),
        RawVerdict::Pass => {}
    }
    let final_body = outbound_resp.body.as_json().cloned().unwrap_or(Value::Null);

    usage::record(
        &state.db,
        &ctx,
        &trace.upstream_model,
        &usage_snapshot,
        "ok",
        None,
        start,
        None, // 非流式无 TTFT 语义
    );

    trace.client_response = final_body.clone();
    trace.usage = usage_snapshot;
    trace.latency_ms = start.elapsed().as_millis() as u64;
    trace::write(state.config.trace_dir.as_deref(), &trace);

    Ok(axum::Json(final_body).into_response())
}

/// 从 RawBody 取出 JSON；文本尝试解析，其余报错。
fn take_json_body(body: RawBody, protocol: Protocol) -> Result<Value> {
    match body {
        RawBody::Json { value } => Ok(value),
        RawBody::Text { text } => serde_json::from_str(&text).map_err(|e| {
            GatewayError::Protocol(moonbridge_core::CoreError::protocol(
                protocol.to_string(),
                format!("报文改写后非法 JSON: {e}"),
            ))
        }),
        RawBody::Empty => Ok(Value::Null),
        RawBody::Binary { .. } => Err(GatewayError::Protocol(moonbridge_core::CoreError::protocol(
            protocol.to_string(),
            "报文为二进制，无法作为 JSON 处理",
        ))),
    }
}

/// 由报文钩子的短路判定构造直接应答。
fn short_circuit(status: u16, headers: Vec<(String, String)>, body: RawBody) -> Response {
    let code = StatusCode::from_u16(status).unwrap_or(StatusCode::OK);
    let body_bytes = match body {
        RawBody::Json { value } => value.to_string().into_bytes(),
        RawBody::Text { text } => text.into_bytes(),
        RawBody::Binary { data } => data,
        RawBody::Empty => Vec::new(),
    };
    let has_ct = headers
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("content-type"));
    let mut builder = Response::builder().status(code);
    if !has_ct {
        builder = builder.header("content-type", "application/json");
    }
    for (k, v) in headers {
        builder = builder.header(k.as_str(), v.as_str());
    }
    builder.body(Body::from(body_bytes)).unwrap()
}
