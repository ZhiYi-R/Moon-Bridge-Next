//! Usage（用量统计）查询 commands。

use std::sync::Arc;

use moonbridge_store::{UsageQuery, UsageRecord, UsageSummary};
use tauri::State;

use crate::commands::CmdResult;
use crate::state::ManagedState;

/// 按条件查询用量记录（model/status 可选，limit 默认 100）。
#[tauri::command]
pub fn usage_query(
    state: State<'_, Arc<ManagedState>>,
    model: Option<String>,
    status: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
) -> CmdResult<Vec<UsageRecord>> {
    let q = UsageQuery {
        model,
        status,
        since: None,
        until: None,
        limit: limit.unwrap_or(100),
        offset: offset.unwrap_or(0),
    };
    Ok(state.db.query_usage(&q)?)
}

/// 用量汇总（总请求数、总 token 等）。
#[tauri::command]
pub fn usage_summary(state: State<'_, Arc<ManagedState>>) -> CmdResult<UsageSummary> {
    Ok(state.db.usage_summary()?)
}
