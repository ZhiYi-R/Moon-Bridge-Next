//! 配额查询 commands，语义 1:1 对齐服务端 `/api/quota/*`。
//!
//! 配额绑定长在 Provider 上（`quota_plugin_ref`/`quota_interval_secs`/`quota_enabled`/
//! `quota_config`），保存/解绑走 provider upsert，这里只暴露查询面：列表、刷新、
//! dry-run。执行引擎在 gateway crate，与网关启停无关——网关未运行时刷新照常可用。

use std::sync::Arc;

use moonbridge_gateway::{list_quota_views, QuotaEngine};
use moonbridge_store::{Provider, ProviderQuotaView, QuotaKeyResult};
use tauri::State;

use crate::commands::CmdResult;
use crate::state::ManagedState;

/// 构造执行引擎（脚本目录与插件脚本走同一套包含性约束）。
fn engine(state: &ManagedState) -> QuotaEngine {
    QuotaEngine::new(state.db.clone(), Some(state.paths.plugins_dir.clone())).with_network_policy(
        moonbridge_gateway::QuotaNetworkPolicy::from_environment(
            state.config().gateway.egress_proxy,
        ),
    )
}

/// 列出全部绑定了配额插件的 Provider 及其最近一次查询结果。
#[tauri::command]
pub fn quota_list(state: State<'_, Arc<ManagedState>>) -> CmdResult<Vec<ProviderQuotaView>> {
    Ok(list_quota_views(&state.db)?)
}

/// 同步刷新一个 Provider 的配额（逐端点各跑一次绑定插件），返回刷新后的视图。
#[tauri::command]
pub async fn quota_refresh(
    state: State<'_, Arc<ManagedState>>,
    provider_key: String,
) -> CmdResult<ProviderQuotaView> {
    let provider = state
        .db
        .get_provider(&provider_key)?
        .ok_or_else(|| format!("上游服务不存在: {provider_key}"))?;
    if provider.quota_plugin_ref.trim().is_empty() {
        return Err(format!("上游服务未绑定配额查询插件: {provider_key}").into());
    }
    Ok(engine(&state).refresh_provider(&provider).await?)
}

/// 串行刷新全部**启用**配额查询的 Provider，返回刷新后全部视图（与 list 同形）。
#[tauri::command]
pub async fn quota_refresh_all(
    state: State<'_, Arc<ManagedState>>,
) -> CmdResult<Vec<ProviderQuotaView>> {
    Ok(engine(&state).refresh_all().await?)
}

/// 以表单里的 Provider 配置 dry-run 一次配额脚本（**不写库、不要求已保存**），
/// 逐端点返回本次结果供编辑表单预览。
#[tauri::command]
pub async fn quota_test(
    state: State<'_, Arc<ManagedState>>,
    provider: Provider,
) -> CmdResult<Vec<QuotaKeyResult>> {
    Ok(engine(&state).test_provider(&provider).await)
}
