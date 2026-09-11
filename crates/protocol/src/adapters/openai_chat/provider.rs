//! OpenAI Chat Completions 上游 Adapter —— 非流式部分。

use async_trait::async_trait;
use http::Method;
use moonbridge_core::{CoreRequest, CoreResponse, Protocol, Result};
use serde_json::{json, Value};

use super::dto;
use super::OpenAiChatAdapter;
use crate::adapter::{ProviderAdapter, ProviderEndpoint, UpstreamRequest};
use crate::adapters::reasoning_effort;
use crate::context::ReqCtx;

#[async_trait]
impl ProviderAdapter for OpenAiChatAdapter {
    fn protocol(&self) -> Protocol {
        Protocol::OpenAiChat
    }

    async fn from_core_request(
        &self,
        _ctx: &ReqCtx,
        req: &CoreRequest,
        endpoint: &ProviderEndpoint,
    ) -> Result<UpstreamRequest> {
        let url = format!("{}{}", endpoint.base_url.trim_end_matches('/'), dto::CHAT_PATH);

        let mut body = req
            .meta
            .get("chat.extra")
            .and_then(|v| v.as_object().cloned())
            .map(Value::Object)
            .unwrap_or_else(|| json!({}));
        let obj = body.as_object_mut().expect("body is object");
        obj.insert("model".to_string(), json!(req.model));
        obj.insert(
            "messages".to_string(),
            json!(dto::core_to_chat_messages(&req.system, &req.messages)),
        );
        obj.insert("stream".to_string(), json!(req.stream));
        // 现代字段名是 max_completion_tokens（o 系模型拒收旧名 max_tokens）。
        // 客户端未设上限时不凭空注入——让上游按自己的默认行为处理。
        if let Some(mt) = req.max_tokens {
            obj.insert("max_completion_tokens".to_string(), json!(mt));
            obj.remove("max_tokens");
        }
        // 流式必须显式请求 usage 终块，否则上游不回传 token 用量，
        // 用量统计/计费对该类请求恒为 0（OpenAI 及兼容实现通用约定）。
        if req.stream {
            obj.insert("stream_options".to_string(), json!({ "include_usage": true }));
        }
        if !req.tools.is_empty() {
            obj.insert("tools".to_string(), json!(dto::core_to_chat_tools(&req.tools)));
        }
        if let Some(tc) = &req.tool_choice {
            obj.insert("tool_choice".to_string(), dto::unparse_tool_choice(tc));
        }
        if let Some(t) = req.temperature {
            obj.insert("temperature".to_string(), json!(t));
        }
        if let Some(p) = req.top_p {
            obj.insert("top_p".to_string(), json!(p));
        }
        if !req.stop.is_empty() {
            obj.insert("stop".to_string(), json!(req.stop));
        }
        // 推理强度：Chat Completions 用 reasoning_effort 字符串枚举表达
        if let Some(effort) = reasoning_effort(req) {
            obj.insert("reasoning_effort".to_string(), json!(effort));
        }

        let mut headers = vec![
            ("content-type".to_string(), "application/json".to_string()),
            ("authorization".to_string(), format!("Bearer {}", endpoint.api_key)),
        ];
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
        let id = raw.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
        let model = raw.get("model").and_then(|v| v.as_str()).unwrap_or_default().to_string();
        let choice = raw
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first())
            .cloned()
            .unwrap_or(Value::Null);
        let (content, stop_reason) = dto::chat_choice_to_core(&choice);
        let usage = raw.get("usage").map(dto::usage_from_chat).unwrap_or_default();

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
    use moonbridge_core::{ContentBlock, Message, Reasoning, Role, StopReason};

    fn endpoint() -> ProviderEndpoint {
        ProviderEndpoint {
            key: "openai".into(),
            protocol: Protocol::OpenAiChat,
            base_url: "https://api.openai.com".into(),
            api_key: "sk-test".into(),
            version: None,
            user_agent: None,
            extra: Default::default(),
        }
    }

    #[tokio::test]
    async fn builds_chat_request() {
        let mut req = CoreRequest::new("gpt-4o");
        req.system.push(ContentBlock::text("Be brief"));
        req.messages.push(Message::text(Role::User, "Hi"));
        req.max_tokens = Some(128);

        let adapter = OpenAiChatAdapter;
        let ctx = ReqCtx::new("r1", Protocol::Anthropic);
        let up = adapter.from_core_request(&ctx, &req, &endpoint()).await.unwrap();

        assert!(up.url.ends_with("/v1/chat/completions"));
        assert_eq!(up.body["messages"][0]["role"], "system");
        assert_eq!(up.body["messages"][0]["content"], "Be brief");
        assert_eq!(up.body["messages"][1]["role"], "user");
        assert_eq!(up.body["max_completion_tokens"], 128);
        assert!(up.body.get("max_tokens").is_none(), "现代字段名是 max_completion_tokens");
        assert!(up.headers.iter().any(|(k, v)| k == "authorization" && v == "Bearer sk-test"));
    }

    /// 回归：`reasoning.effort` 必须传导到上游 `reasoning_effort`。
    /// 历史缺陷是 Core IR 只有入口侧写、四个上游 adapter 全不读，Codex 的推理
    /// 强度意图被静默丢弃。
    #[tokio::test]
    async fn propagates_reasoning_effort() {
        let mut req = CoreRequest::new("o3");
        req.messages.push(Message::text(Role::User, "Hi"));
        req.reasoning = Some(Reasoning {
            effort: Some("high".into()),
            summary: None,
        });
        let adapter = OpenAiChatAdapter;
        let ctx = ReqCtx::new("r1", Protocol::OpenAiChat);
        let up = adapter.from_core_request(&ctx, &req, &endpoint()).await.unwrap();
        assert_eq!(up.body["reasoning_effort"], "high");

        // 未声明时不得凭空长出该字段
        let plain = CoreRequest::new("gpt-4o");
        let up2 = adapter
            .from_core_request(&ctx, &plain, &endpoint())
            .await
            .unwrap();
        assert!(up2.body.get("reasoning_effort").is_none());
    }

    #[tokio::test]
    async fn parses_chat_response() {
        let raw = json!({
            "id": "chatcmpl-2",
            "model": "gpt-4o",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": null, "tool_calls": [
                    { "id": "c9", "type": "function", "function": { "name": "get_time", "arguments": "{\"tz\":\"UTC\"}" } }
                ]},
                "finish_reason": "tool_calls"
            }],
            "usage": { "prompt_tokens": 11, "completion_tokens": 4, "total_tokens": 15 }
        });
        let adapter = OpenAiChatAdapter;
        let ctx = ReqCtx::new("r1", Protocol::Anthropic);
        let resp = adapter.to_core_response(&ctx, raw).await.unwrap();

        assert_eq!(resp.id, "chatcmpl-2");
        assert_eq!(resp.stop_reason, Some(StopReason::ToolUse));
        match &resp.content[0] {
            ContentBlock::ToolUse { id, name, input, .. } => {
                assert_eq!(id, "c9");
                assert_eq!(name, "get_time");
                assert_eq!(input["tz"], "UTC");
            }
            other => panic!("expected tool_use, got {other:?}"),
        }
        assert_eq!(resp.usage.input_tokens, 11);
    }
}
