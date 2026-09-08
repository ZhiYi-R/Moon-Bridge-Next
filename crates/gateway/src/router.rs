//! 模型路由：把客户端请求的模型别名解析为具体 provider + 上游模型 + 端点。
//!
//! 解析优先级（对齐 Moon Bridge）：
//! 1. `model(provider)` 限定名——直接指定 provider；
//! 2. `routes` 别名映射；
//! 3. `offers` 匹配——任一启用的 provider 提供该模型名。

use moonbridge_core::{ModelRef, Protocol};
use moonbridge_protocol::ProviderEndpoint;
use moonbridge_store::{Database, Provider};
use serde_json::Value;

use crate::error::{GatewayError, Result};

/// 路由解析结果。
pub struct ResolvedRoute {
    pub provider_key: String,
    /// 上游实际模型名。
    pub upstream_model: String,
    pub protocol: Protocol,
    /// 端点列表（按序故障转移）。
    pub endpoints: Vec<ProviderEndpoint>,
}

/// 由 store 的 Provider 记录构造协议层端点列表（按配置顺序；故障转移用）。
///
/// 协议绑定在端点上：逐端点解析各自协议，同一 Provider 可混合不同协议的端点。
/// 各端点共享 version/user_agent/extra；api_key 为空时回退沿用前一个非空 Key
/// （便于同一 Provider 的多个镜像端点共用一把密钥）。
pub fn endpoints_from_provider(p: &Provider) -> Result<Vec<ProviderEndpoint>> {
    let extra = match &p.extra {
        Value::Object(m) => m.clone(),
        _ => Default::default(),
    };
    let mut out = Vec::with_capacity(p.endpoints.len());
    let mut last_key = String::new();
    for e in &p.endpoints {
        let protocol = Protocol::parse(&e.protocol).ok_or_else(|| {
            GatewayError::Route(format!(
                "provider {} 端点 '{}' 的协议 '{}' 无法识别",
                p.key, e.base_url, e.protocol
            ))
        })?;
        let api_key = if e.api_key.is_empty() {
            last_key.clone()
        } else {
            last_key = e.api_key.clone();
            e.api_key.clone()
        };
        out.push(ProviderEndpoint {
            key: p.key.clone(),
            protocol,
            base_url: e.base_url.clone(),
            api_key,
            version: p.version.clone(),
            user_agent: p.user_agent.clone(),
            extra: extra.clone(),
        });
    }
    if out.is_empty() {
        return Err(GatewayError::Route(format!(
            "provider {} 未配置任何端点",
            p.key
        )));
    }
    Ok(out)
}

/// 模型路由器（无状态；每次解析直接查库，便于配置热更新即时生效）。
pub struct Router;

impl Router {
    /// 解析模型别名。
    pub fn resolve(db: &Database, model_alias: &str) -> Result<ResolvedRoute> {
        let mref = ModelRef::parse(model_alias);

        // 1. 限定名 model(provider)
        if let Some(provider_key) = &mref.provider {
            return build(db, provider_key, &mref.model).and_then(|opt| {
                opt.ok_or_else(|| {
                    GatewayError::Route(format!(
                        "provider '{provider_key}' 未找到（模型 {}）",
                        mref.model
                    ))
                })
            });
        }

        // 2. routes 别名
        if let Some(route) = db.resolve_route(model_alias)? {
            return build(db, &route.provider_key, &route.model_slug).and_then(|opt| {
                opt.ok_or_else(|| {
                    GatewayError::Route(format!(
                        "路由 '{model_alias}' 指向的 provider '{}' 未找到",
                        route.provider_key
                    ))
                })
            });
        }

        // 3. offers 匹配（遍历启用的 provider）
        for p in db.list_providers()? {
            if !p.enabled {
                continue;
            }
            for offer in db.list_offers(&p.key)? {
                if offer.model_slug == mref.model {
                    if let Some(r) = build(db, &p.key, &mref.model)? {
                        return Ok(r);
                    }
                }
            }
        }

        Err(GatewayError::Route(format!("无法解析模型 '{model_alias}'")))
    }
}

fn build(db: &Database, provider_key: &str, upstream_model: &str) -> Result<Option<ResolvedRoute>> {
    let Some(p) = db.get_provider(provider_key)? else {
        return Ok(None);
    };
    let endpoints = endpoints_from_provider(&p)?;
    // 默认协议取首个端点（路由前 ctx 与流式回退用；实际请求逐端点判定）
    let protocol = endpoints[0].protocol;
    Ok(Some(ResolvedRoute {
        provider_key: p.key.clone(),
        upstream_model: upstream_model.to_string(),
        protocol,
        endpoints,
    }))
}
