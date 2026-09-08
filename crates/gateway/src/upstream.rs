//! 上游请求：reqwest 客户端构建、请求发送、SSE 字节流 → RawChunk 流。

use std::time::Duration;

use futures::{Stream, StreamExt};
use moonbridge_core::Protocol;
use moonbridge_protocol::{RawChunk, UpstreamRequest};

use crate::config::GatewayConfig;
use crate::error::{GatewayError, Result};
use crate::sse;

/// 构建上游 HTTP 客户端（施加 egress 代理；不设总超时以兼容长流式响应）。
pub fn build_client(config: &GatewayConfig) -> Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder().connect_timeout(Duration::from_secs(30));
    if let Some(proxy) = &config.egress_proxy {
        let p = reqwest::Proxy::all(proxy)
            .map_err(|e| GatewayError::Other(format!("egress 代理配置错误: {e}")))?;
        builder = builder.proxy(p);
    }
    builder.build().map_err(Into::into)
}

/// 发送上游请求（非流式与流式共用；调用方按 stream 决定如何消费响应）。
pub async fn send(client: &reqwest::Client, up: &UpstreamRequest) -> Result<reqwest::Response> {
    let mut req = client.request(up.method.clone(), &up.url);
    for (k, v) in &up.headers {
        req = req.header(k.as_str(), v.as_str());
    }
    req = req.json(&up.body);
    Ok(req.send().await?)
}

/// 把上游响应的 SSE 字节流转换为协议中立的 [`RawChunk`] 流。
///
/// 每个 chunk 随后交由 dispatch 触发 `on_upstream_chunk_raw` 钩子，再由上游
/// Adapter 的 `decode` 转为 CoreStreamEvent。
pub fn response_to_chunks(
    resp: reqwest::Response,
    protocol: Protocol,
    provider: Option<String>,
) -> impl Stream<Item = Result<RawChunk>> {
    use eventsource_stream::Eventsource;
    resp.bytes_stream()
        .eventsource()
        .filter_map(move |ev| {
            let provider = provider.clone();
            async move {
                match ev {
                    Ok(e) => {
                        if e.data.is_empty() {
                            return None; // 跳过心跳/空事件
                        }
                        let event = if e.event.is_empty() { None } else { Some(e.event) };
                        Some(Ok(sse::parse_upstream_event(
                            event, &e.data, protocol, provider,
                        )))
                    }
                    Err(err) => Some(Err(GatewayError::Other(format!(
                        "上游 SSE 解析错误: {err}"
                    )))),
                }
            }
        })
}
