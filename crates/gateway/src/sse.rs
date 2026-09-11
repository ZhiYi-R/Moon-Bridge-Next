//! SSE 编解码（协议无关）。
//!
//! 上游方向：把 SSE event 解析为 [`RawChunk`]（data 优先按 JSON 承载）。
//! 客户端方向：把 [`RawChunk`] 序列化回 SSE 文本帧。
//! 这一层只处理 SSE 传输格式，具体协议语义由各 Adapter 的 decode/encode 负责。

use moonbridge_core::Protocol;
use moonbridge_protocol::{ChunkStage, RawBody, RawChunk};

/// 把上游 SSE 的一个 event 解析为 RawChunk。
pub fn parse_upstream_event(
    event: Option<String>,
    data: &str,
    protocol: Protocol,
    provider: Option<String>,
) -> RawChunk {
    let body = if data.trim() == "[DONE]" {
        RawBody::Text {
            text: data.to_string(),
        }
    } else {
        match serde_json::from_str::<serde_json::Value>(data) {
            Ok(v) => RawBody::json(v),
            Err(_) => RawBody::text(data),
        }
    };
    RawChunk {
        stage: ChunkStage::UpstreamChunk,
        protocol,
        provider,
        event,
        data: body,
        raw: data.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_json_event() {
        let c = parse_upstream_event(
            Some("content_block_delta".into()),
            r#"{"type":"content_block_delta","index":0}"#,
            Protocol::Anthropic,
            Some("deepseek".into()),
        );
        assert_eq!(c.event.as_deref(), Some("content_block_delta"));
        assert_eq!(c.data.as_json().unwrap()["index"], 0);
    }

    #[test]
    fn parses_done_as_text() {
        let c = parse_upstream_event(None, "[DONE]", Protocol::OpenAiResponse, None);
        assert!(matches!(c.data, RawBody::Text { .. }));
    }
}
