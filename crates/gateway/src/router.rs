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
#[derive(Debug)]
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
            return build(db, provider_key, &mref.model, None).and_then(|opt| {
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
            return build(db, &route.provider_key, &route.model_slug, None).and_then(|opt| {
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
                    // offer 可绑定特定协议端点：非空则只用该协议的端点故障转移。
                    if let Some(r) =
                        build(db, &p.key, &mref.model, offer.endpoint_protocol.as_deref())?
                    {
                        return Ok(r);
                    }
                }
            }
        }

        Err(GatewayError::Route(format!("无法解析模型 '{model_alias}'")))
    }
}

/// 构造路由结果。`endpoint_protocol` 非空时只保留匹配该协议的端点（供 offer 绑定端点用），
/// 为空则用该 provider 的全部端点（按 idx 故障转移，旧行为）。
fn build(
    db: &Database,
    provider_key: &str,
    upstream_model: &str,
    endpoint_protocol: Option<&str>,
) -> Result<Option<ResolvedRoute>> {
    let Some(p) = db.get_provider(provider_key)? else {
        return Ok(None);
    };
    let mut endpoints = endpoints_from_provider(&p)?;
    // offer 指定了端点协议：筛掉其余协议，只在该协议的端点间故障转移。
    if let Some(want) = endpoint_protocol {
        let Some(target) = Protocol::parse(want) else {
            return Err(GatewayError::Route(format!(
                "offer 绑定的端点协议 '{want}' 无法识别（provider {provider_key} / 模型 {upstream_model}）"
            )));
        };
        endpoints.retain(|e| e.protocol == target);
        if endpoints.is_empty() {
            return Err(GatewayError::Route(format!(
                "provider {provider_key} 无协议为 '{want}' 的端点（模型 {upstream_model} 的 offer 绑定了该协议）"
            )));
        }
    }
    // 默认协议取首个端点（路由前 ctx 与流式回退用；实际请求逐端点判定）
    let protocol = endpoints[0].protocol;
    Ok(Some(ResolvedRoute {
        provider_key: p.key.clone(),
        upstream_model: upstream_model.to_string(),
        protocol,
        endpoints,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use moonbridge_store::{Endpoint, Offer};

    /// 造一个持三种协议端点（anthropic / openai-response / openai-chat）的 provider。
    fn provider_with_three_endpoints() -> Provider {
        Provider {
            key: "zen".into(),
            endpoints: vec![
                Endpoint {
                    protocol: "anthropic".into(),
                    base_url: "https://zen/anthropic".into(),
                    api_key: "k".into(),
                },
                Endpoint {
                    protocol: "openai-response".into(),
                    base_url: "https://zen/openai".into(),
                    api_key: "k".into(),
                },
                Endpoint {
                    protocol: "openai-chat".into(),
                    base_url: "https://zen/chat".into(),
                    api_key: "k".into(),
                },
            ],
            version: None,
            user_agent: None,
            web_search: None,
            extra: Value::Null,
            enabled: true,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn db_with_provider() -> Database {
        let db = Database::open_in_memory().unwrap();
        db.upsert_provider(&provider_with_three_endpoints()).unwrap();
        db
    }

    fn offer(model: &str, endpoint_protocol: Option<&str>) -> Offer {
        Offer {
            provider_key: "zen".into(),
            model_slug: model.into(),
            pricing: None,
            endpoint_protocol: endpoint_protocol.map(str::to_string),
        }
    }

    /// offer 绑定 openai-response：只保留该协议端点，默认协议随之为 OpenAiResponse。
    #[test]
    fn offer_bound_protocol_selects_only_that_endpoint() {
        let db = db_with_provider();
        db.upsert_offer(&offer("muse", Some("openai-response"))).unwrap();

        let r = Router::resolve(&db, "muse").expect("应解析成功");
        assert_eq!(r.endpoints.len(), 1, "只应保留绑定协议的端点");
        assert_eq!(r.protocol, Protocol::OpenAiResponse);
        assert_eq!(r.endpoints[0].base_url, "https://zen/openai");
    }

    /// offer 不绑定端点协议：保留全部端点，默认协议为首个端点（旧行为）。
    #[test]
    fn offer_without_binding_keeps_all_endpoints() {
        let db = db_with_provider();
        db.upsert_offer(&offer("muse", None)).unwrap();

        let r = Router::resolve(&db, "muse").expect("应解析成功");
        assert_eq!(r.endpoints.len(), 3, "未绑定则保留全部端点");
        assert_eq!(r.protocol, Protocol::Anthropic, "默认协议为 idx=0 端点");
    }

    /// offer 绑定的协议在该 provider 无对应端点：明确报错，而非静默错端点。
    #[test]
    fn offer_bound_to_absent_protocol_errors() {
        let db = db_with_provider();
        db.upsert_offer(&offer("muse", Some("google-genai"))).unwrap();

        let err = Router::resolve(&db, "muse").unwrap_err();
        assert!(
            matches!(&err, GatewayError::Route(m) if m.contains("google-genai")),
            "应报「无该协议端点」错误: {err:?}"
        );
    }

    /// 绑定协议的别名无法识别：报错而非当作裸名。
    #[test]
    fn offer_bound_to_unknown_protocol_name_errors() {
        let db = db_with_provider();
        db.upsert_offer(&offer("muse", Some("nonsense-proto"))).unwrap();

        let err = Router::resolve(&db, "muse").unwrap_err();
        assert!(
            matches!(&err, GatewayError::Route(m) if m.contains("无法识别")),
            "应报协议无法识别错误: {err:?}"
        );
    }

    /// endpoint_protocol 支持 Protocol::parse 的别名（如 "gemini" → google-genai）。
    #[test]
    fn offer_bound_protocol_accepts_aliases() {
        let db = Database::open_in_memory().unwrap();
        let mut p = provider_with_three_endpoints();
        p.endpoints.push(Endpoint {
            protocol: "google-genai".into(),
            base_url: "https://zen/gemini".into(),
            api_key: "k".into(),
        });
        db.upsert_provider(&p).unwrap();
        // offer 用别名 "gemini"，应归一到 google-genai 端点
        db.upsert_offer(&offer("g", Some("gemini"))).unwrap();

        let r = Router::resolve(&db, "g").expect("应解析成功");
        assert_eq!(r.endpoints.len(), 1);
        assert_eq!(r.protocol, Protocol::GoogleGenai);
    }
}
