//! 请求 trace 落盘：把一次请求各阶段的报文快照写入文件系统（不入库）。
//!
//! 目录结构：`<trace_dir>/<session>/<model>/<created_at>-<short_id>.json`，与架构
//! 文档约定一致。仅当 [`crate::config::GatewayConfig::trace_dir`] 有值时写入；
//! 任何失败只告警，绝不影响主请求链路。

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use moonbridge_core::Usage;
use serde::Serialize;
use serde_json::Value;

/// 一次请求的 trace 快照（camelCase，直接供前端 Traces 页读取展示）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceRecord {
    pub request_id: String,
    /// 起始时间（epoch 毫秒）。
    pub created_at: u64,
    pub session_id: Option<String>,
    pub model_alias: String,
    pub upstream_model: String,
    pub provider_key: String,
    pub client_protocol: String,
    pub upstream_protocol: String,
    pub stream: bool,
    /// `ok` / `error`。
    pub status: String,
    pub latency_ms: u64,
    pub usage: Usage,
    /// 客户端入站请求体（经入站报文钩子后）。
    pub client_request: Value,
    /// 上游出站请求快照 `{ method, url, headers, body }`（经出站报文钩子后）。
    pub upstream_request: Value,
    /// 上游入站响应体（非流式）；流式为 `null`。
    pub upstream_response: Value,
    /// 回写客户端的响应体（非流式）；流式为 `null`。
    pub client_response: Value,
    pub error: Option<String>,
}

/// 当前 epoch 毫秒。
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 清洗路径片段中的非法/控制字符，避免越权写入或非法文件名。
fn sanitize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() {
            out.push('_');
        } else {
            out.push(c);
        }
    }
    let trimmed = out.trim().trim_matches('.');
    if trimmed.is_empty() {
        "_".to_string()
    } else {
        trimmed.chars().take(80).collect()
    }
}

/// 写入一条 trace。`trace_dir` 为 `None`/空或写入失败时静默跳过（仅告警）。
pub fn write(trace_dir: Option<&str>, rec: &TraceRecord) {
    let Some(dir) = trace_dir else { return };
    if dir.trim().is_empty() {
        return;
    }
    let session = sanitize(rec.session_id.as_deref().unwrap_or("_no_session"));
    let model = sanitize(if rec.model_alias.is_empty() {
        "_unknown"
    } else {
        &rec.model_alias
    });
    let target = Path::new(dir).join(session).join(model);
    if let Err(e) = std::fs::create_dir_all(&target) {
        tracing::warn!(error = %e, "创建 trace 目录失败");
        return;
    }
    let short = rec
        .request_id
        .split('-')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("req");
    let file = target.join(format!("{}-{}.json", rec.created_at, short));
    match serde_json::to_vec_pretty(rec) {
        Ok(bytes) => {
            if let Err(e) = std::fs::write(&file, bytes) {
                tracing::warn!(error = %e, "写入 trace 文件失败");
            }
        }
        Err(e) => tracing::warn!(error = %e, "序列化 trace 失败"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample(session: Option<&str>, model: &str) -> TraceRecord {
        TraceRecord {
            request_id: "abcdef00-1111-2222-3333-444455556666".into(),
            created_at: 1_700_000_000_000,
            session_id: session.map(String::from),
            model_alias: model.into(),
            upstream_model: model.into(),
            provider_key: "mock".into(),
            client_protocol: "openai-chat".into(),
            upstream_protocol: "anthropic".into(),
            stream: false,
            status: "ok".into(),
            latency_ms: 12,
            usage: Usage::default(),
            client_request: json!({ "model": model }),
            upstream_request: json!({ "url": "https://x" }),
            upstream_response: Value::Null,
            client_response: Value::Null,
            error: None,
        }
    }

    #[test]
    fn sanitize_strips_illegal_chars() {
        assert_eq!(sanitize("a/b\\c:d"), "a_b_c_d");
        assert_eq!(sanitize(".."), "_");
        assert_eq!(sanitize(""), "_");
        assert_eq!(sanitize("sess-1"), "sess-1");
    }

    #[test]
    fn writes_trace_file_under_session_model() {
        let dir = std::env::temp_dir().join(format!("mb-trace-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let rec = sample(Some("sess-1"), "claude-x");
        write(Some(dir.to_str().unwrap()), &rec);

        let expected = dir.join("sess-1").join("claude-x");
        let entries: Vec<_> = std::fs::read_dir(&expected).unwrap().map(|e| e.unwrap().path()).collect();
        assert_eq!(entries.len(), 1, "应写入一个 trace 文件");
        let content = std::fs::read_to_string(&entries[0]).unwrap();
        assert!(content.contains("\"requestId\""), "camelCase 序列化: {content}");
        assert!(content.contains("abcdef00"), "文件名/内容含短 id");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_dir_is_noop() {
        // trace_dir 为 None / 空时应静默跳过，不 panic
        write(None, &sample(None, "m"));
        write(Some("  "), &sample(None, "m"));
    }
}
