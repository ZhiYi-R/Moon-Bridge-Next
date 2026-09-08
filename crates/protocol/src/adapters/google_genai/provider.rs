//! Google Generative AI (Gemini) 上游 Adapter —— 非流式部分。
//!
//! `from_core_request`：CoreRequest → Gemini `generateContent` /
//! `streamGenerateContent` 请求（contents/systemInstruction/tools/generationConfig）。
//! `to_core_response`：Gemini 响应对象 → CoreResponse（candidates 解析）。
//!
//! 与入口侧（[`super::client`]）共享 [`super::dto`] 的 parts/tools/usage 转换，
//! 保证 Gemini 既作入口又作上游时语义一致。

use async_trait::async_trait;
use http::Method;
use moonbridge_core::{ContentBlock, CoreRequest, CoreResponse, Protocol, Result, StopReason};
use serde_json::{json, Value};

use super::dto;
use super::GoogleGenAiAdapter;
use crate::adapter::{ProviderAdapter, ProviderEndpoint, UpstreamRequest};
use crate::context::ReqCtx;

#[async_trait]
impl ProviderAdapter for GoogleGenAiAdapter {
    fn protocol(&self) -> Protocol {
        Protocol::GoogleGenai
    }

    async fn from_core_request(
        &self,
        _ctx: &ReqCtx,
        req: &CoreRequest,
        endpoint: &ProviderEndpoint,
    ) -> Result<UpstreamRequest> {
        let version = endpoint.version.as_deref().unwrap_or(dto::DEFAULT_VERSION);
        let path = if req.stream {
            dto::stream_path(version, &req.model)
        } else {
            dto::generate_path(version, &req.model)
        };
        let url = format!("{}{}", endpoint.base_url.trim_end_matches('/'), path);

        let mut body = json!({ "contents": dto::core_to_contents(&req.messages) });
        let obj = body.as_object_mut().expect("body is object");
        if let Some(si) = dto::core_to_system_instruction(&req.system, &req.messages) {
            obj.insert("systemInstruction".to_string(), si);
        }
        if let Some(tools) = dto::core_to_tools(&req.tools) {
            obj.insert("tools".to_string(), tools);
        }
        if let Some(tc) = dto::core_to_tool_config(req.tool_choice.as_ref()) {
            obj.insert("toolConfig".to_string(), tc);
        }
        if let Some(gc) = dto::core_to_generation_config(req) {
            obj.insert("generationConfig".to_string(), gc);
        }

        // Gemini 官方以 x-goog-api-key 鉴权；兼容网关亦可改用 Bearer（extra.auth=bearer）
        let use_bearer = endpoint
            .extra
            .get("auth")
            .and_then(|v| v.as_str())
            .map(|s| s.eq_ignore_ascii_case("bearer"))
            .unwrap_or(false);
        let mut headers = vec![("content-type".to_string(), "application/json".to_string())];
        if use_bearer {
            headers.push(("authorization".to_string(), format!("Bearer {}", endpoint.api_key)));
        } else {
            headers.push(("x-goog-api-key".to_string(), endpoint.api_key.clone()));
        }
        if let Some(ua) = &endpoint.user_agent {
            headers.push(("user-agent".to_string(), ua.clone()));
        }

        Ok(UpstreamRequest {
            method: Method::POST,
            url,
            headers,
            body,
            stream: req.stream,
        })
    }

    async fn to_core_response(&self, _ctx: &ReqCtx, raw: Value) -> Result<CoreResponse> {
        let id = raw.get("responseId").and_then(|v| v.as_str()).unwrap_or_default().to_string();
        let model = raw
            .get("modelVersion")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();

        let candidate = raw
            .get("candidates")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first())
            .cloned()
            .unwrap_or(Value::Null);
        let (content, finish) = dto::candidate_to_core(&candidate);

        // Gemini 对函数调用同样返回 STOP；含 tool_use 时归一为 ToolUse
        let stop_reason = if content.iter().any(|b| matches!(b, ContentBlock::ToolUse { .. })) {
            Some(StopReason::ToolUse)
        } else {
            finish.or(Some(StopReason::EndTurn))
        };

        let usage = raw.get("usageMetadata").map(dto::usage_from_gemini).unwrap_or_default();

        Ok(CoreResponse {
            id,
            model,
            content,
            stop_reason,
            usage,
            ext: Default::default(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use moonbridge_core::{Message, Role, Tool, ToolChoice};

    fn endpoint() -> ProviderEndpoint {
        ProviderEndpoint {
            key: "google".into(),
            protocol: Protocol::GoogleGenai,
            base_url: "https://generativelanguage.googleapis.com".into(),
            api_key: "AIza-test".into(),
            version: Some("v1beta".into()),
            user_agent: None,
            extra: Default::default(),
        }
    }

    #[tokio::test]
    async fn builds_generate_content_request() {
        let mut req = CoreRequest::new("gemini-2.0-flash");
        req.system.push(ContentBlock::text("Be brief"));
        req.messages.push(Message::text(Role::User, "Hi"));
        req.tools.push(Tool {
            name: "get_time".into(),
            description: Some("tz time".into()),
            input_schema: json!({ "type": "object" }),
            ext: Default::default(),
        });
        req.tool_choice = Some(ToolChoice::Auto);
        req.max_tokens = Some(256);
        req.temperature = Some(0.5);

        let adapter = GoogleGenAiAdapter;
        let ctx = ReqCtx::new("r1", Protocol::OpenAiChat);
        let up = adapter.from_core_request(&ctx, &req, &endpoint()).await.unwrap();

        assert!(up.url.ends_with("/v1beta/models/gemini-2.0-flash:generateContent"));
        assert_eq!(up.body["systemInstruction"]["parts"][0]["text"], "Be brief");
        assert_eq!(up.body["contents"][0]["role"], "user");
        assert_eq!(up.body["contents"][0]["parts"][0]["text"], "Hi");
        assert_eq!(up.body["tools"][0]["functionDeclarations"][0]["name"], "get_time");
        assert_eq!(up.body["toolConfig"]["functionCallingConfig"]["mode"], "AUTO");
        assert_eq!(up.body["generationConfig"]["maxOutputTokens"], 256);
        assert!(up.headers.iter().any(|(k, v)| k == "x-goog-api-key" && v == "AIza-test"));
    }

    #[tokio::test]
    async fn stream_request_uses_sse_path() {
        let mut req = CoreRequest::new("gemini-2.0-flash");
        req.messages.push(Message::text(Role::User, "Hi"));
        req.stream = true;
        let adapter = GoogleGenAiAdapter;
        let ctx = ReqCtx::new("r1", Protocol::OpenAiChat);
        let up = adapter.from_core_request(&ctx, &req, &endpoint()).await.unwrap();
        assert!(up.url.contains(":streamGenerateContent?alt=sse"));
        assert!(up.stream);
    }

    #[tokio::test]
    async fn parses_generate_content_response() {
        let raw = json!({
            "candidates": [{
                "content": { "role": "model", "parts": [{ "text": "Hello there" }] },
                "finishReason": "STOP",
                "index": 0
            }],
            "usageMetadata": { "promptTokenCount": 9, "candidatesTokenCount": 3, "totalTokenCount": 12 },
            "modelVersion": "gemini-2.0-flash",
            "responseId": "resp-abc"
        });
        let adapter = GoogleGenAiAdapter;
        let ctx = ReqCtx::new("r1", Protocol::OpenAiChat);
        let resp = adapter.to_core_response(&ctx, raw).await.unwrap();

        assert_eq!(resp.id, "resp-abc");
        assert_eq!(resp.model, "gemini-2.0-flash");
        assert_eq!(resp.stop_reason, Some(StopReason::EndTurn));
        match &resp.content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "Hello there"),
            other => panic!("expected text, got {other:?}"),
        }
        assert_eq!(resp.usage.input_tokens, 9);
        assert_eq!(resp.usage.output_tokens, 3);
    }

    #[tokio::test]
    async fn function_call_response_maps_to_tool_use() {
        let raw = json!({
            "candidates": [{
                "content": { "role": "model", "parts": [{ "functionCall": { "name": "get_time", "args": {"tz":"UTC"} } }] },
                "finishReason": "STOP"
            }]
        });
        let adapter = GoogleGenAiAdapter;
        let ctx = ReqCtx::new("r1", Protocol::OpenAiChat);
        let resp = adapter.to_core_response(&ctx, raw).await.unwrap();
        assert_eq!(resp.stop_reason, Some(StopReason::ToolUse));
        assert!(matches!(resp.content[0], ContentBlock::ToolUse { .. }));
    }
}
