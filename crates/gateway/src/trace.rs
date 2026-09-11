//! 请求 trace 落盘：把一次请求各阶段的报文快照写入文件系统（不入库）。
//!
//! 目录结构：`<trace_dir>/<session>/<model>/<created_at>-<short_id>.json`，与架构
//! 文档约定一致。仅当 [`crate::config::GatewayConfig::trace_dir`] 有值时写入；
//! 任何失败只告警，绝不影响主请求链路。

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use moonbridge_core::Usage;
use serde::Serialize;
use serde_json::Value;

/// trace 对外暴露的用量快照（camelCase）。
///
/// `moonbridge_core::Usage` **有意**保持 snake_case——Lua 插件经 mlua serde 直接读写它
/// （见 `docs/architecture.md` §3 命名约定）。而 serde 的 `rename_all` **不作用于嵌套
/// 类型**，所以把 `Usage` 原样塞进 camelCase 的 `TraceRecord`，落盘就成了
/// `"usage": {"input_tokens": …}`；前端按 `usage.inputTokens` 读取拿到 `undefined`，
/// 再被 `formatTokens` 的 `n ?? 0` 兜成「0」——真实 trace 的 token 全显示为 0 且无报错
/// （已实证）。这里做一次显式映射，使落盘 JSON 符合「面向前端的 DTO 统一 camelCase」。
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_tokens: u32,
    pub cache_write_tokens: u32,
    pub reasoning_tokens: u32,
}

impl From<Usage> for TraceUsage {
    fn from(u: Usage) -> Self {
        TraceUsage {
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
            cache_read_tokens: u.cache_read_tokens,
            cache_write_tokens: u.cache_write_tokens,
            reasoning_tokens: u.reasoning_tokens,
        }
    }
}

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
    /// `ok` / `error` / `aborted`（流未读尽、或插件主动中止）/ `short_circuit`（插件代答）。
    pub status: String,
    pub latency_ms: u64,
    /// 首字延迟（毫秒）；仅流式请求有值，非流式 / 插件代答为 None。
    pub ttft_ms: Option<u64>,
    pub usage: TraceUsage,
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

/// `trace_record_bodies=false` 时落盘前抹除全部报文体。
///
/// `upstream_request` 是 `{ method, url, headers, body }` 快照——URL/headers 已脱敏
/// 但 body 含完整上游请求报文（全部 prompt/消息），属于敏感数据，必须一并抹除；
/// 只清 `client_request` 会留下旁路（流式路径曾因此泄漏）。
pub fn strip_bodies(t: &mut TraceRecord) {
    t.client_request = Value::Null;
    t.client_response = Value::Null;
    t.upstream_response = Value::Null;
    if let Some(obj) = t.upstream_request.as_object_mut() {
        obj.insert("body".to_string(), Value::Null);
    }
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

/// 写入一条 trace，并按保留条数清理最老的记录。
///
/// `trace_dir` 为 `None`/空或写入失败时静默跳过（仅告警）；`retention` 为保留的
/// 最大文件数（按修改时间从老到新），`0` 表示不清理。
pub fn write(trace_dir: Option<&str>, rec: &TraceRecord, retention: usize) {
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
    prune(Path::new(dir), retention);
}

/// 按 mtime 保留最近 `retention` 条 trace，删除超出的最老文件，并清掉空目录。
fn prune(dir: &Path, retention: usize) {
    if retention == 0 {
        return;
    }
    let mut files: Vec<(PathBuf, std::time::SystemTime)> = Vec::new();
    collect_json_files(dir, &mut files);
    if files.len() <= retention {
        return;
    }
    files.sort_by_key(|(_, m)| *m); // 最老的在前
    for (path, _) in files.iter().take(files.len() - retention) {
        if let Err(e) = std::fs::remove_file(path) {
            tracing::warn!(error = %e, path = %path.display(), "清理过期 trace 失败");
        }
    }
    remove_empty_dirs(dir);
}

fn collect_json_files(dir: &Path, out: &mut Vec<(PathBuf, std::time::SystemTime)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_json_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "json") {
            let mtime = entry
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            out.push((path, mtime));
        }
    }
}

/// 自底向上尝试删除空目录（非空时 remove_dir 失败即静默保留）。
fn remove_empty_dirs(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            remove_empty_dirs(&path);
            let _ = std::fs::remove_dir(&path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn prunes_oldest_beyond_retention_and_keeps_newest() {
        let dir = std::env::temp_dir().join(format!("mb-trace-prune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("s")).unwrap();
        for i in 0..3 {
            std::fs::write(dir.join("s").join(format!("{i}.json")), "{}").unwrap();
            std::thread::sleep(std::time::Duration::from_millis(20)); // 保证 mtime 可区分
        }
        prune(&dir, 2);
        assert!(!dir.join("s").join("0.json").exists(), "最老的应被清理");
        assert!(dir.join("s").join("1.json").exists());
        assert!(dir.join("s").join("2.json").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn zero_retention_disables_prune() {
        let dir = std::env::temp_dir().join(format!("mb-trace-keep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.json"), "{}").unwrap();
        prune(&dir, 0);
        assert!(dir.join("a.json").exists(), "0 表示不清理");
        let _ = std::fs::remove_dir_all(&dir);
    }

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
            ttft_ms: None,
            usage: TraceUsage::default(),
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
        write(Some(dir.to_str().unwrap()), &rec, 0);

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
        write(None, &sample(None, "m"), 0);
        write(Some("  "), &sample(None, "m"), 0);
    }

    /// 回归：`usage` 必须随外层一起序列化为 camelCase。
    ///
    /// 历史缺陷是 `TraceRecord` 上 `rename_all = "camelCase"` **不会**作用到嵌套的
    /// `moonbridge_core::Usage`（它无 rename，线上是 snake_case），于是前端
    /// `usage.inputTokens` 恒为 `undefined`，被 `formatTokens(n ?? 0)` 渲染成 0
    /// ——真实 trace 的 token 全部静默显示为 0。
    #[test]
    fn usage_is_camel_case_and_never_leaks_snake_case() {
        let mut rec = sample(Some("sess-1"), "claude-x");
        rec.usage = Usage {
            input_tokens: 111,
            output_tokens: 222,
            cache_read_tokens: 33,
            cache_write_tokens: 44,
            reasoning_tokens: 55,
        }
        .into();

        let v = serde_json::to_value(&rec).unwrap();
        let u = &v["usage"];
        assert_eq!(u["inputTokens"], 111, "camelCase 键: {u}");
        assert_eq!(u["outputTokens"], 222);
        assert_eq!(u["cacheReadTokens"], 33);
        assert_eq!(u["cacheWriteTokens"], 44);
        assert_eq!(u["reasoningTokens"], 55);
        for snake in [
            "input_tokens",
            "output_tokens",
            "cache_read_tokens",
            "cache_write_tokens",
            "reasoning_tokens",
        ] {
            assert!(u.get(snake).is_none(), "不得泄漏 snake_case 键 {snake}: {u}");
        }
        // 外层仍须是 camelCase
        assert!(v.get("requestId").is_some());
        assert!(v.get("latencyMs").is_some());
    }
}
