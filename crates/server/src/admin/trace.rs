//! Trace（请求报文快照）浏览 handlers。
//!
//! trace 由网关在 `GatewayConfig.trace_dir`（= `data_dir/traces`）下按
//! `<session>/<model>/<created_at>-<id>.json` 落盘。此处提供只读浏览：列举、读取
//! 单条、删除单条。所有路径均在 `trace_dir` 内做规范化校验，防止路径穿越。

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use axum::extract::{Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{AdminState, ApiError, ApiResult};

/// trace 列表项（轻量元数据，不含报文体）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceEntry {
    /// 会话目录名。
    pub session: String,
    /// 模型目录名。
    pub model: String,
    /// 文件名。
    pub file_name: String,
    /// 相对 `trace_dir` 的路径（用于读取 / 删除）。
    pub rel_path: String,
    /// 文件修改时间（epoch 毫秒）。
    pub modified_at: u64,
    /// 文件字节数。
    pub size: u64,
}

fn to_ms(t: std::io::Result<std::time::SystemTime>) -> u64 {
    t.ok()
        .and_then(|st| st.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// `GET /api/traces?limit=` 的查询参数。
#[derive(Debug, Deserialize)]
pub struct TraceListParams {
    pub limit: Option<usize>,
}

/// 列举 trace（按修改时间倒序，最多 `limit` 条，默认 500）。
pub async fn trace_list(
    State(state): State<AdminState>,
    Query(params): Query<TraceListParams>,
) -> ApiResult<Json<Vec<TraceEntry>>> {
    let root = state.paths.trace_dir.clone();
    let max = params.limit.unwrap_or(500);
    let mut out = Vec::new();
    if !root.exists() {
        return Ok(Json(out));
    }
    // 两层目录：<session>/<model>/<file>.json
    let sessions = std::fs::read_dir(&root).map_err(|e| ApiError::internal(e.to_string()))?;
    for sess in sessions.flatten() {
        let sess_path = sess.path();
        if !sess_path.is_dir() {
            continue;
        }
        let session = sess_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let models = match std::fs::read_dir(&sess_path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        for mdl in models.flatten() {
            let mdl_path = mdl.path();
            if !mdl_path.is_dir() {
                continue;
            }
            let model = mdl_path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let files = match std::fs::read_dir(&mdl_path) {
                Ok(f) => f,
                Err(_) => continue,
            };
            for f in files.flatten() {
                let fp = f.path();
                if fp.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                let meta = match f.metadata() {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                let file_name = fp
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                let rel_path = format!("{session}/{model}/{file_name}");
                out.push(TraceEntry {
                    session: session.clone(),
                    model: model.clone(),
                    file_name,
                    rel_path,
                    modified_at: to_ms(meta.modified()),
                    size: meta.len(),
                });
            }
        }
    }
    // Reverse ⇒ 新的在前（后接 truncate 取最近 N 条）
    out.sort_by_key(|e| std::cmp::Reverse(e.modified_at));
    out.truncate(max);
    Ok(Json(out))
}

/// `GET`/`DELETE /api/trace?path=` 的查询参数（相对 `trace_dir` 的路径）。
#[derive(Debug, Deserialize)]
pub struct TracePathParams {
    pub path: Option<String>,
}

/// 把相对路径解析为 `trace_dir` 内的绝对路径，并做穿越校验。
fn resolve_within(root: &Path, rel: &str) -> ApiResult<PathBuf> {
    let root_canon = root
        .canonicalize()
        .map_err(|_| ApiError::not_found("trace 目录不存在"))?;
    // 拒绝绝对路径与显式上跳，避免穿越
    let candidate = root_canon.join(rel.trim_start_matches('/'));
    let canon = candidate
        .canonicalize()
        .map_err(|_| ApiError::not_found(format!("trace 不存在: {rel}")))?;
    if !canon.starts_with(&root_canon) {
        return Err(ApiError::bad_request("非法的 trace 路径"));
    }
    Ok(canon)
}

/// 取出必填的 `path` 查询参数并做穿越校验。
fn resolve_query_path(state: &AdminState, params: &TracePathParams) -> ApiResult<PathBuf> {
    let rel = params
        .path
        .as_deref()
        .ok_or_else(|| ApiError::bad_request("缺少 path 参数"))?;
    resolve_within(&state.paths.trace_dir, rel)
}

/// 读取单条 trace 的完整 JSON。
pub async fn trace_read(
    State(state): State<AdminState>,
    Query(params): Query<TracePathParams>,
) -> ApiResult<Json<Value>> {
    let full = resolve_query_path(&state, &params)?;
    let text = std::fs::read_to_string(&full).map_err(|e| ApiError::internal(e.to_string()))?;
    let value: Value =
        serde_json::from_str(&text).map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(value))
}

/// 删除单条 trace 文件。
pub async fn trace_delete(
    State(state): State<AdminState>,
    Query(params): Query<TracePathParams>,
) -> ApiResult<Json<Value>> {
    let full = resolve_query_path(&state, &params)?;
    std::fs::remove_file(&full).map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;

    /// 进程 pid 隔离的临时 trace 根（不引入 tempfile 依赖）。
    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "moonbridge-server-trace-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn resolve_within_accepts_nested_relative_path() {
        let root = temp_root("resolve-ok");
        let file = root.join("sess").join("model");
        std::fs::create_dir_all(&file).unwrap();
        let target = file.join("1-abc.json");
        std::fs::write(&target, "{}").unwrap();

        let got = resolve_within(&root, "sess/model/1-abc.json").unwrap();
        assert_eq!(got, target.canonicalize().unwrap());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn resolve_within_rejects_parent_traversal() {
        let root = temp_root("resolve-up");
        // 根之外造一个真实存在的诱饵文件：`../` 上跳若能命中它即为穿越成功
        let sibling_name = format!(
            "moonbridge-server-trace-outside-{}.json",
            std::process::id()
        );
        let outside = root.parent().unwrap().join(&sibling_name);
        std::fs::write(&outside, "{}").unwrap();

        let err = resolve_within(&root, &format!("../{sibling_name}")).unwrap_err();
        assert_eq!(err.status, StatusCode::BAD_REQUEST, "上跳应 400");
        assert!(!err.message.is_empty());
        // 诱饵文件必须仍然存在（未被读到、更未被删）
        assert!(outside.exists(), "不得触及根之外的文件");
        let _ = std::fs::remove_file(&outside);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn resolve_within_rejects_absolute_path_escape() {
        let root = temp_root("resolve-abs");
        // 绝对路径必须被收敛到 root 之内；`/etc/passwd` 落在 root 下并不存在 ⇒ 拒绝
        let err = resolve_within(&root, "/etc/passwd").unwrap_err();
        assert!(
            err.status.is_client_error(),
            "绝对路径穿越必须被 4xx 拒绝: {}",
            err.status
        );
        // 直接给出的绝对路径同样不得逃逸
        let outside = root.parent().unwrap().join(format!(
            "moonbridge-server-trace-abs-{}.json",
            std::process::id()
        ));
        std::fs::write(&outside, "{}").unwrap();
        let err = resolve_within(&root, &outside.to_string_lossy()).unwrap_err();
        assert_eq!(err.status, StatusCode::NOT_FOUND);
        assert!(outside.exists(), "不得触及根之外的文件");
        let _ = std::fs::remove_file(&outside);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn resolve_within_reports_missing_root_and_missing_file() {
        let missing_root = std::env::temp_dir().join(format!(
            "moonbridge-server-trace-absent-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&missing_root);
        let err = resolve_within(&missing_root, "a/b.json").unwrap_err();
        assert_eq!(err.status, StatusCode::NOT_FOUND);

        let root = temp_root("resolve-missing-file");
        let err = resolve_within(&root, "nope/x.json").unwrap_err();
        assert_eq!(err.status, StatusCode::NOT_FOUND);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn resolve_query_path_requires_path_param() {
        let root = temp_root("resolve-param");
        let paths = crate::config::AppPaths::resolve(root.join("cfg"), root.clone());
        let state = AdminState {
            db: std::sync::Arc::new(moonbridge_store::Database::open_in_memory().unwrap()),
            paths,
            config: std::sync::Arc::new(
                std::sync::RwLock::new(crate::config::AppConfig::default()),
            ),
            persisted_config: std::sync::Arc::new(std::sync::RwLock::new(
                crate::config::AppConfig::default(),
            )),
            lifecycle: std::sync::Arc::new(crate::lifecycle::Lifecycle::default()),
            admin_token: std::sync::Arc::new("t".to_string()),
            catalog: reqwest::Client::new(),
        };
        let err = resolve_query_path(&state, &TracePathParams { path: None }).unwrap_err();
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
        assert!(err.message.contains("path"), "{}", err.message);
        let _ = std::fs::remove_dir_all(&root);
    }
}
