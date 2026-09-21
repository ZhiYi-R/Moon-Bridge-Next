//! 配额查询管理 API（`/api/quota/*`），语义 1:1 对齐桌面端
//! `src-tauri/src/commands/quota.rs`。
//!
//! 配额绑定长在 Provider 上，保存/解绑走 `/api/providers/*`；这里只暴露查询面。
//! 执行引擎在 gateway crate（[`moonbridge_gateway::QuotaEngine`]），与网关的
//! 插件注册表解耦。

use axum::extract::{Path, State};
use axum::Json;
use moonbridge_gateway::{list_quota_views, QuotaEngine};
use moonbridge_store::{Provider, ProviderQuotaView, QuotaKeyResult};

use super::{require, AdminState, ApiError, ApiResult};

/// 构造执行引擎：脚本目录与插件脚本走同一套包含性约束（见 `parse_script_ref`）。
fn engine(state: &AdminState) -> QuotaEngine {
    QuotaEngine::new(state.db.clone(), Some(state.paths.plugins_dir.clone())).with_network_policy(
        moonbridge_gateway::QuotaNetworkPolicy::from_environment(
            state.config().gateway.egress_proxy,
        ),
    )
}

/// GET /api/quota：全部绑定了配额插件的 Provider 视图（绑定信息 + 逐端点最近结果）。
pub async fn quota_list(
    State(state): State<AdminState>,
) -> ApiResult<Json<Vec<ProviderQuotaView>>> {
    Ok(Json(list_quota_views(&state.db)?))
}

/// POST /api/quota/:key/refresh：同步执行该 Provider 绑定的配额脚本，
/// 返回刷新后的视图。
pub async fn quota_refresh(
    State(state): State<AdminState>,
    Path(key): Path<String>,
) -> ApiResult<Json<ProviderQuotaView>> {
    let provider = require(state.db.get_provider(&key)?, || {
        format!("上游服务不存在: {key}")
    })?;
    if provider.quota_plugin_ref.trim().is_empty() {
        return Err(ApiError::bad_request(format!(
            "上游服务未绑定配额查询插件: {key}"
        )));
    }
    Ok(Json(engine(&state).refresh_provider(&provider).await?))
}

/// POST /api/quota/refresh：串行刷新全部**启用**配额查询的 Provider，返回刷新后
/// 全部视图（与 `quota_list` 同形，前端可直接整体替换）。
pub async fn quota_refresh_all(
    State(state): State<AdminState>,
) -> ApiResult<Json<Vec<ProviderQuotaView>>> {
    Ok(Json(engine(&state).refresh_all().await?))
}

/// POST /api/quota/test：以请求体里的 Provider 配置 dry-run 一次配额脚本
/// （**不写库、不要求已保存**），逐端点返回本次结果供编辑表单预览。
pub async fn quota_test(
    State(state): State<AdminState>,
    Json(provider): Json<Provider>,
) -> ApiResult<Json<Vec<QuotaKeyResult>>> {
    Ok(Json(engine(&state).test_provider(&provider).await))
}
