//! 管理 API（`/api/*`）：路由装配、统一错误类型与 handlers。
//!
//! 语义 1:1 对齐桌面端 Tauri commands（`src-tauri/src/commands/`），仅把 IPC 换成
//! REST：成功响应返回 JSON（无返回值的操作返回 `null`），错误统一
//! `{"message": string}` + 状态码（400 用户错误 / 404 不存在 / 500 内部）。
//!
//! 认证：全部 `/api/*` 需要 `Authorization: Bearer <admin_token>`；token 由启动参数
//! 强制提供（见 [`crate::args`]）。

pub mod catalog;
pub mod plugin;
pub mod quota;
pub mod trace;

use std::sync::{Arc, RwLock};

use axum::extract::{DefaultBodyLimit, Path, Query, Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use moonbridge_store::{
    Database, ModelDef, Offer, Provider, ProviderCost, Route, Setting, UsageQuery, UsageRecord,
    UsageSummary,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::{AppConfig, AppPaths};

/// admin 请求体上限（10MB）：够传插件脚本与大段 JSON 配置，又不至于被过大的 body 耗尽内存。
const MAX_BODY_BYTES: usize = 10 * 1024 * 1024;

#[derive(Clone)]
pub struct AdminState {
    /// SQLite 存储句柄（与网关共用同一连接）。
    pub db: Arc<Database>,
    /// 关键路径集合（trace / 插件脚本包含性校验用）。
    pub paths: AppPaths,
    /// 运行时引导配置，包含只驻留内存的启动覆盖。
    pub config: Arc<RwLock<AppConfig>>,
    pub persisted_config: Arc<RwLock<AppConfig>>,
    pub lifecycle: Arc<crate::lifecycle::Lifecycle>,
    /// 管理 API 的 Bearer token（只驻留内存）。
    pub admin_token: Arc<String>,
    /// 目录拉取用 HTTP 客户端（30s 超时，直连不走 egress）。
    pub catalog: reqwest::Client,
}

impl AdminState {
    pub fn config(&self) -> AppConfig {
        self.config.read().unwrap().clone()
    }

    pub fn replace_config(&self, mut config: AppConfig) -> anyhow::Result<()> {
        let requested_token = config
            .gateway
            .auth_token
            .take()
            .map(|token| token.trim().to_string())
            .filter(|token| !token.is_empty());
        if let Some(token) = requested_token.as_deref() {
            if !moonbridge_gateway::auth::token_is_valid(token)
                || token == self.admin_token.as_str()
            {
                anyhow::bail!(
                    "gateway token 必须是不含空白的可打印 ASCII 字符，且不同于 admin token"
                );
            }
        }
        let mut runtime = self.config.write().unwrap();
        let mut persisted = self.persisted_config.write().unwrap();
        let mut snapshot = config.clone();
        snapshot.gateway.auth_token = requested_token
            .clone()
            .or_else(|| persisted.gateway.auth_token.clone());
        if snapshot.gateway.auth_token.as_deref() == Some(self.admin_token.as_str()) {
            snapshot.gateway.auth_token = None;
        }
        config.gateway.auth_token = requested_token.or_else(|| runtime.gateway.auth_token.clone());
        snapshot.save(&self.paths.config_file)?;
        *persisted = snapshot;
        *runtime = config;
        Ok(())
    }
}

/// 管理 API 错误：状态码 + `{"message": ...}`。
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    /// 400：请求方错误（参数非法、路径越界、启用条件未满足等）。
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }

    /// 404：目标不存在。
    pub fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: message.into(),
        }
    }

    /// 500：服务端内部错误（存储、IO、序列化等）。
    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorMessage {
                message: self.message,
            }),
        )
            .into_response()
    }
}

impl From<moonbridge_store::StoreError> for ApiError {
    fn from(e: moonbridge_store::StoreError) -> Self {
        ApiError::internal(e.to_string())
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        ApiError::internal(format!("{e:#}"))
    }
}

impl From<String> for ApiError {
    fn from(message: String) -> Self {
        ApiError::internal(message)
    }
}

/// 未实现的 `/api` 子路径：返回 JSON 404，而不是被静态托管的 SPA fallback 吞成 200 HTML。
async fn api_not_found() -> ApiError {
    ApiError::not_found("未找到该管理 API 端点")
}

#[derive(Debug, Serialize)]
struct ErrorMessage {
    message: String,
}

type ApiResult<T> = std::result::Result<T, ApiError>;

/// 把 `Option` 变成「存在则返回，否则 404」。
fn require<T>(value: Option<T>, what: impl FnOnce() -> String) -> ApiResult<T> {
    value.ok_or_else(|| ApiError::not_found(what()))
}

/// 构建管理 API 路由（尚未合并到网关路由）。
pub fn router(state: AdminState) -> Router {
    Router::new()
        // ---- 网关生命周期 ----
        .route("/api/gateway/restart", post(gateway_restart))
        .route("/api/gateway/status", get(gateway_status))
        // ---- 应用信息与引导配置 ----
        .route("/api/app/info", get(app_info))
        .route("/api/config", get(config_get).put(config_set))
        // ---- provider ----
        .route("/api/providers", get(provider_list).put(provider_save))
        .route(
            "/api/providers/:key",
            get(provider_get).delete(provider_delete),
        )
        .route("/api/providers/:key/offers", get(offer_list))
        .route("/api/offers", put(offer_save))
        .route("/api/providers/:key/offers/:slug", delete(offer_delete))
        // ---- model ----
        .route("/api/models", get(model_list).put(model_save))
        .route("/api/models/:slug", get(model_get).delete(model_delete))
        // ---- catalog ----
        .route("/api/catalog", get(catalog::catalog_fetch))
        .route("/api/catalog/import", post(catalog::catalog_import))
        // ---- route ----
        .route("/api/routes", get(route_list).put(route_save))
        .route("/api/routes/:alias", get(route_get).delete(route_delete))
        // ---- plugin ----
        .route(
            "/api/plugins",
            get(plugin::plugin_list).put(plugin::plugin_save),
        )
        .route("/api/plugins/import", post(plugin::plugin_import))
        .route(
            "/api/plugins/:name",
            get(plugin::plugin_get).delete(plugin::plugin_delete),
        )
        .route(
            "/api/plugins/:name/script",
            get(plugin::plugin_read_script).put(plugin::plugin_write_script),
        )
        .route("/api/plugins/:name/bindings", get(plugin::binding_list))
        // ---- binding ----
        .route(
            "/api/bindings",
            get(plugin::binding_list_by_scope).put(plugin::binding_save),
        )
        .route(
            "/api/bindings/:pluginName/:scope/:scopeKey",
            delete(plugin::binding_delete),
        )
        // ---- usage ----
        .route("/api/usage", get(usage_query))
        .route("/api/usage/summary", get(usage_summary))
        .route("/api/usage/cost-by-provider", get(usage_cost_by_provider))
        // ---- quota（配额查询）----
        .route("/api/quota", get(quota::quota_list))
        .route("/api/quota/:key/refresh", post(quota::quota_refresh))
        .route("/api/quota/refresh", post(quota::quota_refresh_all))
        .route("/api/quota/test", post(quota::quota_test))
        // ---- trace ----
        .route("/api/traces", get(trace::trace_list))
        .route(
            "/api/trace",
            get(trace::trace_read).delete(trace::trace_delete),
        )
        // ---- settings ----
        .route("/api/settings", get(settings_list))
        .route(
            "/api/settings/:key",
            get(settings_get).put(settings_set).delete(settings_delete),
        )
        // 回退：`/api/*` 下未定义的路径一律 JSON 404，避免落到 SPA fallback 返回 index.html。
        .route(
            "/api/*rest",
            get(api_not_found)
                .post(api_not_found)
                .put(api_not_found)
                .delete(api_not_found),
        )
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .route_layer(middleware::from_fn_with_state(state.clone(), auth))
        .with_state(state)
}

/// Bearer 认证中间件：`Authorization: Bearer <admin_token>`，失败 401。
async fn auth(State(state): State<AdminState>, req: Request, next: Next) -> Response {
    if !moonbridge_gateway::auth::bearer_matches(req.headers(), state.admin_token.as_str()) {
        return ApiError {
            status: StatusCode::UNAUTHORIZED,
            message: "无效或缺失的 Bearer token".to_string(),
        }
        .into_response();
    }
    next.run(req).await
}

// ───────────────────────── 网关生命周期 ─────────────────────────

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayStatus {
    pub running: bool,
    pub addr: String,
    pub error: Option<String>,
    pub phase: crate::lifecycle::Phase,
}

async fn gateway_restart(State(state): State<AdminState>) -> impl IntoResponse {
    state.lifecycle.request_restart();
    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "message": "restarting" })),
    )
}

async fn gateway_status(State(state): State<AdminState>) -> ApiResult<Json<GatewayStatus>> {
    let snapshot = state.lifecycle.snapshot();
    Ok(Json(GatewayStatus {
        running: snapshot.running,
        addr: snapshot.addr,
        error: snapshot.error,
        phase: snapshot.phase,
    }))
}

// ───────────────────────── 应用信息与引导配置 ─────────────────────────

/// 应用运行信息（版本 + 关键路径 + 运行模式）。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub version: String,
    pub db_path: String,
    pub config_path: String,
    pub data_dir: String,
    pub plugins_dir: String,
    pub trace_dir: String,
    pub mode: String,
}

async fn app_info(State(state): State<AdminState>) -> ApiResult<Json<AppInfo>> {
    let p = &state.paths;
    Ok(Json(AppInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        db_path: p.db_path.to_string_lossy().to_string(),
        config_path: p.config_file.to_string_lossy().to_string(),
        data_dir: p.data_dir.to_string_lossy().to_string(),
        plugins_dir: p.plugins_dir.to_string_lossy().to_string(),
        trace_dir: p.trace_dir.to_string_lossy().to_string(),
        mode: "server".to_string(),
    }))
}

async fn config_get(State(state): State<AdminState>) -> ApiResult<Json<AppConfig>> {
    let mut config = state.config();
    config.gateway.auth_token = None;
    Ok(Json(config))
}

async fn config_set(
    State(state): State<AdminState>,
    Json(config): Json<AppConfig>,
) -> ApiResult<Json<Value>> {
    if let Some(token) = config
        .gateway
        .auth_token
        .as_deref()
        .map(str::trim)
        .filter(|token| !token.is_empty())
    {
        if !moonbridge_gateway::auth::token_is_valid(token) || token == state.admin_token.as_str() {
            return Err(ApiError::bad_request(
                "gateway token 必须是不含空白的可打印 ASCII 字符，且不同于 admin token",
            ));
        }
    }
    state.replace_config(config)?;
    Ok(Json(Value::Null))
}

// ───────────────────────── provider / model / offer ─────────────────────────

async fn provider_list(State(state): State<AdminState>) -> ApiResult<Json<Vec<Provider>>> {
    Ok(Json(state.db.list_providers()?))
}

async fn provider_get(
    State(state): State<AdminState>,
    Path(key): Path<String>,
) -> ApiResult<Json<Provider>> {
    let p = require(state.db.get_provider(&key)?, || {
        format!("provider 不存在: {key}")
    })?;
    Ok(Json(p))
}

async fn provider_save(
    State(state): State<AdminState>,
    Json(provider): Json<Provider>,
) -> ApiResult<Json<Value>> {
    state.db.upsert_provider(&provider)?;
    Ok(Json(Value::Null))
}

async fn provider_delete(
    State(state): State<AdminState>,
    Path(key): Path<String>,
) -> ApiResult<Json<Value>> {
    state.db.delete_provider(&key)?;
    Ok(Json(Value::Null))
}

async fn model_list(State(state): State<AdminState>) -> ApiResult<Json<Vec<ModelDef>>> {
    Ok(Json(state.db.list_models()?))
}

async fn model_get(
    State(state): State<AdminState>,
    Path(slug): Path<String>,
) -> ApiResult<Json<ModelDef>> {
    let m = require(state.db.get_model(&slug)?, || format!("模型不存在: {slug}"))?;
    Ok(Json(m))
}

async fn model_save(
    State(state): State<AdminState>,
    Json(model): Json<ModelDef>,
) -> ApiResult<Json<Value>> {
    state.db.upsert_model(&model)?;
    Ok(Json(Value::Null))
}

async fn model_delete(
    State(state): State<AdminState>,
    Path(slug): Path<String>,
) -> ApiResult<Json<Value>> {
    state.db.delete_model(&slug)?;
    Ok(Json(Value::Null))
}

async fn offer_list(
    State(state): State<AdminState>,
    Path(key): Path<String>,
) -> ApiResult<Json<Vec<Offer>>> {
    Ok(Json(state.db.list_offers(&key)?))
}

async fn offer_save(
    State(state): State<AdminState>,
    Json(offer): Json<Offer>,
) -> ApiResult<Json<Value>> {
    state.db.upsert_offer(&offer)?;
    Ok(Json(Value::Null))
}

async fn offer_delete(
    State(state): State<AdminState>,
    Path((key, slug)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    state.db.delete_offer(&key, &slug)?;
    Ok(Json(Value::Null))
}

// ───────────────────────── 路由别名 ─────────────────────────

async fn route_list(State(state): State<AdminState>) -> ApiResult<Json<Vec<Route>>> {
    Ok(Json(state.db.list_routes()?))
}

async fn route_get(
    State(state): State<AdminState>,
    Path(alias): Path<String>,
) -> ApiResult<Json<Route>> {
    let r = require(state.db.get_route(&alias)?, || {
        format!("路由不存在: {alias}")
    })?;
    Ok(Json(r))
}

async fn route_save(
    State(state): State<AdminState>,
    Json(route): Json<Route>,
) -> ApiResult<Json<Value>> {
    state.db.upsert_route(&route)?;
    Ok(Json(Value::Null))
}

async fn route_delete(
    State(state): State<AdminState>,
    Path(alias): Path<String>,
) -> ApiResult<Json<Value>> {
    state.db.delete_route(&alias)?;
    Ok(Json(Value::Null))
}

// ───────────────────────── 用量 ─────────────────────────

/// 用量查询参数（camelCase，全部可选）。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageParams {
    pub model: Option<String>,
    pub provider_key: Option<String>,
    pub status: Option<String>,
    pub since: Option<i64>,
    pub until: Option<i64>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

async fn usage_query(
    State(state): State<AdminState>,
    Query(params): Query<UsageParams>,
) -> ApiResult<Json<Vec<UsageRecord>>> {
    let q = UsageQuery {
        model: params.model,
        provider_key: params.provider_key,
        status: params.status,
        since: params.since,
        until: params.until,
        limit: params.limit.unwrap_or(100),
        offset: params.offset.unwrap_or(0),
    };
    Ok(Json(state.db.query_usage(&q)?))
}

async fn usage_summary(
    State(state): State<AdminState>,
    Query(params): Query<UsageParams>,
) -> ApiResult<Json<UsageSummary>> {
    Ok(Json(
        state.db.usage_summary_range(params.since, params.until)?,
    ))
}

/// 按 provider 汇总消耗（余额页本地等值额度对照上游配额）。
async fn usage_cost_by_provider(
    State(state): State<AdminState>,
    Query(params): Query<UsageParams>,
) -> ApiResult<Json<Vec<ProviderCost>>> {
    Ok(Json(state.db.usage_cost_by_provider(params.since)?))
}

// ───────────────────────── 设置 ─────────────────────────

async fn settings_list(State(state): State<AdminState>) -> ApiResult<Json<Vec<Setting>>> {
    Ok(Json(state.db.list_settings()?))
}

async fn settings_get(
    State(state): State<AdminState>,
    Path(key): Path<String>,
) -> ApiResult<Json<Value>> {
    Ok(Json(state.db.get_setting(&key)?.unwrap_or(Value::Null)))
}

async fn settings_set(
    State(state): State<AdminState>,
    Path(key): Path<String>,
    Json(value): Json<Value>,
) -> ApiResult<Json<Value>> {
    state.db.set_setting(&key, &value)?;
    Ok(Json(Value::Null))
}

async fn settings_delete(
    State(state): State<AdminState>,
    Path(key): Path<String>,
) -> ApiResult<Json<Value>> {
    state.db.delete_setting(&key)?;
    Ok(Json(Value::Null))
}
