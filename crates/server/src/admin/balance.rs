//! 余额&健康看板管理 API（`/api/balance/*`），语义 1:1 对齐桌面端
//! `src-tauri/src/commands/balance.rs`。
//!
//! 卡片配置与最近一次结果永远成对返回（[`BalanceCardView`]）：前端一次拿到卡片
//! 与它当前展示的余额，不需要再发第二次请求。执行引擎在 gateway crate
//! （[`moonbridge_gateway::BalanceEngine`]），与网关的插件注册表解耦。

use axum::extract::{Path, Query, State};
use axum::Json;
use moonbridge_gateway::{list_views, BalanceEngine};
use moonbridge_store::{BalanceCard, BalanceCardView, BalanceKeyResult};
use serde_json::Value;
use std::collections::HashMap;

use super::{require, AdminState, ApiError, ApiResult};

/// 构造执行引擎：脚本目录与插件脚本走同一套包含性约束（见 `parse_script_ref`）。
fn engine(state: &AdminState) -> BalanceEngine {
    BalanceEngine::new(state.db.clone(), Some(state.paths.plugins_dir.clone()))
}

/// GET /api/balance/cards
pub async fn balance_card_list(
    State(state): State<AdminState>,
) -> ApiResult<Json<Vec<BalanceCardView>>> {
    Ok(Json(list_views(&state.db)?))
}

/// PUT /api/balance/cards：upsert 一张卡片（定时间隔的夹逼在 DAO 落库前施加）。
pub async fn balance_card_save(
    State(state): State<AdminState>,
    Json(card): Json<BalanceCard>,
) -> ApiResult<Json<Value>> {
    if card.key.trim().is_empty() {
        return Err(ApiError::bad_request("卡片 key 不能为空"));
    }
    state.db.upsert_balance_card(&card)?;
    Ok(Json(Value::Null))
}

/// DELETE /api/balance/cards/:key（联动删除该卡的查询结果）。
pub async fn balance_card_delete(
    State(state): State<AdminState>,
    Path(key): Path<String>,
) -> ApiResult<Json<Value>> {
    require(state.db.get_balance_card(&key)?, || {
        format!("余额卡片不存在: {key}")
    })?;
    state.db.delete_balance_card(&key)?;
    Ok(Json(Value::Null))
}

/// POST /api/balance/cards/:key/refresh：同步执行该卡脚本，返回刷新后的视图。
///
/// 可选 query `key_index=N`：只重跑该 key（逐 key 拆卡后前端单卡刷新用）；
/// 缺省时整卡全量重跑。
pub async fn balance_card_refresh(
    State(state): State<AdminState>,
    Path(key): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> ApiResult<Json<BalanceCardView>> {
    let card = require(state.db.get_balance_card(&key)?, || {
        format!("余额卡片不存在: {key}")
    })?;
    let key_index = params
        .get("key_index")
        .map(|s| s.parse::<i64>())
        .transpose()
        .map_err(|_| ApiError::bad_request("key_index 必须是整数"))?;
    Ok(Json(engine(&state).refresh_card(&card, key_index).await))
}

/// POST /api/balance/refresh：串行刷新全部**启用**的卡片，返回刷新后**全部**卡片的
/// 列表（与 `balance_card_list` 同形，前端可直接整体替换）。
pub async fn balance_refresh_all(
    State(state): State<AdminState>,
) -> ApiResult<Json<Vec<BalanceCardView>>> {
    Ok(Json(engine(&state).refresh_all().await))
}

/// POST /api/balance/test：以请求体里的卡片配置 dry-run 一次脚本（**不写库、不要求
/// 卡片已保存**），逐 key 返回本次结果供编辑表单预览。body 语义同 `balance_card_save`。
pub async fn balance_card_test(
    State(state): State<AdminState>,
    Json(card): Json<BalanceCard>,
) -> ApiResult<Json<Vec<BalanceKeyResult>>> {
    Ok(Json(engine(&state).test_card(&card).await))
}
