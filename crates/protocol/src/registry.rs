//! Adapter 注册表：按 `Protocol` 索引入口/上游 Adapter（含流式）。
//!
//! gateway 启动时装配 `Registry`，dispatch 时按入口协议取 ClientAdapter、
//! 按上游协议取 ProviderAdapter。

use std::collections::HashMap;
use std::sync::Arc;

use moonbridge_core::Protocol;

use crate::adapter::{ClientAdapter, ClientStreamAdapter, ProviderAdapter, ProviderStreamAdapter};

#[derive(Default)]
pub struct Registry {
    clients: HashMap<Protocol, Arc<dyn ClientAdapter>>,
    client_streams: HashMap<Protocol, Arc<dyn ClientStreamAdapter>>,
    providers: HashMap<Protocol, Arc<dyn ProviderAdapter>>,
    provider_streams: HashMap<Protocol, Arc<dyn ProviderStreamAdapter>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_client(&mut self, a: Arc<dyn ClientAdapter>) {
        self.clients.insert(a.protocol(), a);
    }
    pub fn register_client_stream(&mut self, a: Arc<dyn ClientStreamAdapter>) {
        self.client_streams.insert(a.protocol(), a);
    }
    pub fn register_provider(&mut self, a: Arc<dyn ProviderAdapter>) {
        self.providers.insert(a.protocol(), a);
    }
    pub fn register_provider_stream(&mut self, a: Arc<dyn ProviderStreamAdapter>) {
        self.provider_streams.insert(a.protocol(), a);
    }

    pub fn client(&self, p: Protocol) -> Option<&Arc<dyn ClientAdapter>> {
        self.clients.get(&p)
    }
    pub fn client_stream(&self, p: Protocol) -> Option<&Arc<dyn ClientStreamAdapter>> {
        self.client_streams.get(&p)
    }
    pub fn provider(&self, p: Protocol) -> Option<&Arc<dyn ProviderAdapter>> {
        self.providers.get(&p)
    }
    pub fn provider_stream(&self, p: Protocol) -> Option<&Arc<dyn ProviderStreamAdapter>> {
        self.provider_streams.get(&p)
    }

    pub fn client_len(&self) -> usize {
        self.clients.len()
    }
    pub fn provider_len(&self) -> usize {
        self.providers.len()
    }
}

/// 四种协议（OpenAI Responses / Anthropic / OpenAI Chat / Google GenAI）均实现
/// 四象限，故每个 Adapter 同时注册到入口/上游 × 非流式/流式四张表，构成
/// 4×4 全矩阵（任意入口协议 → 任意上游协议）。
pub fn builtin_registry() -> Registry {
    use crate::adapters::{
        anthropic::AnthropicAdapter, google_genai::GoogleGenAiAdapter,
        openai_chat::OpenAiChatAdapter, openai_responses::OpenAiResponsesAdapter,
    };

    let mut reg = Registry::new();

    macro_rules! register_all {
        ($adapter:expr) => {{
            let a = Arc::new($adapter);
            reg.register_client(a.clone());
            reg.register_client_stream(a.clone());
            reg.register_provider(a.clone());
            reg.register_provider_stream(a);
        }};
    }

    register_all!(OpenAiResponsesAdapter);
    register_all!(AnthropicAdapter);
    register_all!(OpenAiChatAdapter);
    register_all!(GoogleGenAiAdapter);

    reg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_registry_covers_full_matrix() {
        let reg = builtin_registry();
        assert_eq!(reg.client_len(), 4, "四种入口协议均应注册");
        assert_eq!(reg.provider_len(), 4, "四种上游协议均应注册");
        for p in Protocol::all() {
            assert!(reg.client(p).is_some(), "{p} 缺入口 Adapter");
            assert!(reg.client_stream(p).is_some(), "{p} 缺入口流式 Adapter");
            assert!(reg.provider(p).is_some(), "{p} 缺上游 Adapter");
            assert!(reg.provider_stream(p).is_some(), "{p} 缺上游流式 Adapter");
        }
    }
}
