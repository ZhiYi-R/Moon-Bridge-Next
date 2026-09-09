//! Trace（请求报文快照）浏览 commands。
//!
//! trace 由网关在 `GatewayConfig.trace_dir`（= `app_data_dir/traces`）下按
//! `<session>/<model>/<created_at>-<id>.json` 落盘。此处提供只读浏览：列举、读取
//! 单条、删除单条。所有路径均在 `trace_dir` 内做规范化校验，防止路径穿越。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use serde::Serialize;
use serde_json::Value;
use tauri::State;

use crate::commands::CmdResult;
use crate::state::ManagedState;

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
    /// 相对 `trace_dir` 的路径（用于 `trace_read` / `trace_delete`）。
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

/// 列举 trace（按修改时间倒序，最多 `limit` 条，默认 500）。
#[tauri::command]
pub fn trace_list(state: State<'_, Arc<ManagedState>>, limit: Option<usize>) -> CmdResult<Vec<TraceEntry>> {
    let root = state.paths.trace_dir.clone();
    let max = limit.unwrap_or(500);
    let mut out = Vec::new();
    if !root.exists() {
        return Ok(out);
    }
    // 两层目录：<session>/<model>/<file>.json
    let sessions = std::fs::read_dir(&root).map_err(|e| e.to_string())?;
    for sess in sessions.flatten() {
        let sess_path = sess.path();
        if !sess_path.is_dir() {
            continue;
        }
        let session = sess_path.file_name().unwrap_or_default().to_string_lossy().to_string();
        let models = match std::fs::read_dir(&sess_path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        for mdl in models.flatten() {
            let mdl_path = mdl.path();
            if !mdl_path.is_dir() {
                continue;
            }
            let model = mdl_path.file_name().unwrap_or_default().to_string_lossy().to_string();
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
                let file_name = fp.file_name().unwrap_or_default().to_string_lossy().to_string();
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
    Ok(out)
}

/// 把相对路径解析为 `trace_dir` 内的绝对路径，并做穿越校验。
fn resolve_within(root: &Path, rel: &str) -> CmdResult<PathBuf> {
    let root_canon = root
        .canonicalize()
        .map_err(|_| "trace 目录不存在".to_string())?;
    // 拒绝绝对路径与显式上跳，避免穿越
    let candidate = root_canon.join(rel.trim_start_matches('/'));
    let canon = candidate
        .canonicalize()
        .map_err(|_| format!("trace 不存在: {rel}"))?;
    if !canon.starts_with(&root_canon) {
        return Err("非法的 trace 路径".to_string().into());
    }
    Ok(canon)
}

/// 读取单条 trace 的完整 JSON。
#[tauri::command]
pub fn trace_read(state: State<'_, Arc<ManagedState>>, rel_path: String) -> CmdResult<Value> {
    let root = state.paths.trace_dir.clone();
    let full = resolve_within(&root, &rel_path)?;
    let text = std::fs::read_to_string(&full).map_err(|e| e.to_string())?;
    let value: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    Ok(value)
}

/// 删除单条 trace 文件。
#[tauri::command]
pub fn trace_delete(state: State<'_, Arc<ManagedState>>, rel_path: String) -> CmdResult<()> {
    let root = state.paths.trace_dir.clone();
    let full = resolve_within(&root, &rel_path)?;
    std::fs::remove_file(&full).map_err(|e| e.to_string())?;
    Ok(())
}
