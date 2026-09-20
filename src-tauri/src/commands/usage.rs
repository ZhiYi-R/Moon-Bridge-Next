//! Usage（用量统计）查询 commands。

use std::sync::Arc;

use moonbridge_store::{UsageQuery, UsageRecord, UsageSummary};
use tauri::State;

use crate::commands::CmdResult;
use crate::state::ManagedState;

/// 按条件查询用量记录（model/provider_key/status/since/until 可选，limit 默认 100）。
///
/// 参数逐项平铺是 IPC 契约的一部分——前端 `invoke("usage_query", {...})` 直接传扁平
/// 对象（`ui/src/lib/api.ts`）。收拢成结构体会改变 invoke 载荷形状、需同步改 UI，
/// 故此处保留多参数签名并带理由放行。
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub fn usage_query(
    state: State<'_, Arc<ManagedState>>,
    model: Option<String>,
    provider_key: Option<String>,
    status: Option<String>,
    since: Option<i64>,
    until: Option<i64>,
    limit: Option<i64>,
    offset: Option<i64>,
) -> CmdResult<Vec<UsageRecord>> {
    let q = UsageQuery {
        model,
        provider_key,
        status,
        since,
        until,
        limit: limit.unwrap_or(100),
        offset: offset.unwrap_or(0),
    };
    Ok(state.db.query_usage(&q)?)
}

/// 用量汇总（总请求数、总 token 等）；since/until 限定 created_at 秒级范围（含两端）。
#[tauri::command]
pub fn usage_summary(
    state: State<'_, Arc<ManagedState>>,
    since: Option<i64>,
    until: Option<i64>,
) -> CmdResult<UsageSummary> {
    Ok(state.db.usage_summary_range(since, until)?)
}
