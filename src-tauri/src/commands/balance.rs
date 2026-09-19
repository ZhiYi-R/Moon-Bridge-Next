//! 余额&健康看板 commands，语义 1:1 对齐服务端 `/api/balance/*`。
//!
//! 卡片配置与最近一次结果永远成对返回（[`BalanceCardView`]）：前端一次拿到卡片
//! 与它当前展示的余额。脚本目录与插件共用 `plugins_dir` 约束；查询引擎在 gateway
//! crate，与网关启停无关——网关未运行时刷新照常可用。

use std::sync::Arc;

use moonbridge_gateway::{list_views, view_of, BalanceEngine};
use moonbridge_store::{BalanceCard, BalanceCardView, BalanceKeyResult};
use tauri::State;

use crate::commands::CmdResult;
use crate::state::ManagedState;

/// 构造执行引擎（脚本目录与插件脚本走同一套包含性约束）。
fn engine(state: &ManagedState) -> BalanceEngine {
    BalanceEngine::new(state.db.clone(), Some(state.paths.plugins_dir.clone())).with_network_policy(
        moonbridge_gateway::BalanceNetworkPolicy::from_environment(
            state.config().gateway.egress_proxy,
        ),
    )
}

/// 列出全部卡片及其最近一次查询结果。
#[tauri::command]
pub fn balance_card_list(state: State<'_, Arc<ManagedState>>) -> CmdResult<Vec<BalanceCardView>> {
    Ok(list_views(&state.db)?)
}

/// 新增或更新一张卡片（定时间隔的夹逼在 DAO 落库前施加）。
#[tauri::command]
pub fn balance_card_save(state: State<'_, Arc<ManagedState>>, card: BalanceCard) -> CmdResult<()> {
    if card.key.trim().is_empty() {
        return Err("卡片 key 不能为空".to_string().into());
    }
    state.db.upsert_balance_card(&card)?;
    Ok(())
}

/// 删除一张卡片（联动删除其查询结果）。
#[tauri::command]
pub fn balance_card_delete(state: State<'_, Arc<ManagedState>>, key: String) -> CmdResult<()> {
    if state.db.get_balance_card(&key)?.is_none() {
        return Err(format!("余额卡片不存在: {key}").into());
    }
    state.db.delete_balance_card(&key)?;
    Ok(())
}

/// 同步刷新单张卡片，返回刷新后的视图。`key_index` 为 `Some` 时只重跑该 key
/// （逐 key 拆卡后前端单卡刷新用）；`None` 时整卡全量重跑。
#[tauri::command]
pub async fn balance_card_refresh(
    state: State<'_, Arc<ManagedState>>,
    key: String,
    key_index: Option<i64>,
) -> CmdResult<BalanceCardView> {
    let view = view_of(&state.db, &key)?.ok_or_else(|| format!("余额卡片不存在: {key}"))?;
    Ok(engine(&state).refresh_card(&view.card, key_index).await?)
}

/// 串行刷新全部**启用**的卡片，返回刷新后**全部**卡片的列表（与 list 同形）。
#[tauri::command]
pub async fn balance_refresh_all(
    state: State<'_, Arc<ManagedState>>,
) -> CmdResult<Vec<BalanceCardView>> {
    Ok(engine(&state).refresh_all().await?)
}

/// 以表单里的卡片配置 dry-run 一次脚本（**不写库、不要求卡片已保存**），逐 key
/// 返回本次结果供编辑表单预览。
#[tauri::command]
pub async fn balance_card_test(
    state: State<'_, Arc<ManagedState>>,
    card: BalanceCard,
) -> CmdResult<Vec<BalanceKeyResult>> {
    Ok(engine(&state).test_card(&card).await)
}
