//! Adapter 契约：入口/上游 × 非流式/流式 的四象限接口。
//!
//! 所有 Adapter 以 Core IR 为中心进行转换：入口 Adapter（Client*）在
//! 「客户端协议 ⇄ Core」间转换，上游 Adapter（Provider*）在「Core ⇄ 上游协议」
//! 间转换。这样 N 个入口 + M 个上游只需 N + M 个 Adapter，而非 N × M。

use async_trait::async_trait;
use http::Method;
use moonbridge_core::{CoreRequest, CoreResponse, CoreStreamEvent, Map, Protocol, Result, Usage};
use serde_json::Value;

use crate::context::ReqCtx;
use crate::raw::RawChunk;

/// 上游 provider 端点信息，由 gateway 从 store 配置构造后传给 ProviderAdapter。
#[derive(Debug, Clone)]
pub struct ProviderEndpoint {
    /// provider 唯一 key。
    pub key: String,
    /// 上游协议。
    pub protocol: Protocol,
    /// 基础 URL（如 `https://api.anthropic.com`）。
    pub base_url: String,
    /// API Key（已解密）。
    pub api_key: String,
    /// 协议版本头（如 Anthropic 的 `2023-06-01`）。
    pub version: Option<String>,
    /// 自定义 User-Agent。
    pub user_agent: Option<String>,
    /// 协议特定额外字段。
    pub extra: Map,
}

impl ProviderEndpoint {
    /// 构造一个最小端点。
    pub fn new(
        key: impl Into<String>,
        protocol: Protocol,
        base_url: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Self {
        ProviderEndpoint {
            key: key.into(),
            protocol,
            base_url: base_url.into(),
            api_key: api_key.into(),
            version: None,
            user_agent: None,
            extra: Map::new(),
        }
    }
}

/// Adapter 把 CoreRequest 转成上游协议后的「待发送请求」。
#[derive(Debug, Clone)]
pub struct UpstreamRequest {
    pub method: Method,
    pub url: String,
    pub headers: Vec<(String, String)>,
    /// JSON 请求体（绝大多数 LLM API 为 JSON）。
    pub body: Value,
    pub stream: bool,
}

/// 入口协议 ↔ Core（非流式）。例：OpenAI Responses 请求 → CoreRequest。
#[async_trait]
pub trait ClientAdapter: Send + Sync {
    /// 该 Adapter 处理的入口协议。
    fn protocol(&self) -> Protocol;
    /// 客户端原始请求 JSON → CoreRequest。
    async fn to_core_request(&self, ctx: &ReqCtx, raw: Value) -> Result<CoreRequest>;
    /// CoreResponse → 客户端协议响应 JSON。
    //
    // `from_core_*` / `to_core_*` 表达的是 **Core ↔ 协议的转换方向**、与配对方法对称，
    // 不是构造函数；且 Adapter 经 `Arc<dyn …>` 动态派发，`&self` 无法去除。
    #[allow(clippy::wrong_self_convention)]
    async fn from_core_response(&self, ctx: &ReqCtx, resp: CoreResponse) -> Result<Value>;
}

/// Core 流事件 → 入口协议 SSE（流式）。
#[async_trait]
pub trait ClientStreamAdapter: Send + Sync {
    fn protocol(&self) -> Protocol;
    /// 把一个 Core 流事件编码为 0..n 个客户端 SSE chunk。
    ///
    /// gateway 会在写出前对每个 chunk 触发 `on_client_chunk_raw` 钩子。
    /// `st` 为每流状态（gateway 每个流式请求持有一份）：部分协议需跨事件
    /// 决策（如 anthropic 推理块惰性开启，避免产生空 thinking 块）。
    fn encode(
        &self,
        ctx: &ReqCtx,
        ev: &CoreStreamEvent,
        st: &mut StreamEncodeState,
    ) -> Result<Vec<RawChunk>>;
}

/// encode 的每流状态。adapter 本体为并发共享单例，可变状态由 gateway
/// 按请求持有，encode 保持可重入。
#[derive(Default)]
pub struct StreamEncodeState {
    /// 已向客户端开启（content_block_start 已发出）的块索引。
    pub open_blocks: std::collections::HashSet<usize>,
    /// 逐 MessageDelta 取最大值合并出的累计用量——上游常分段上报
    /// （Anthropic 在 message_start 给 input、message_delta 给 output），
    /// 收尾事件须发合并值，否则客户端只见半份。
    pub usage_acc: Usage,
    /// BlockStart 记录下的起始块（ToolUse 元数据等）；收尾事件（如
    /// responses 的 output_item.done）需要还原完整 item 时读取。
    pub blocks: std::collections::HashMap<usize, moonbridge_core::ContentBlock>,
    /// 逐块累积的增量载荷（Text 文本 / ToolInput 的 partial_json），
    /// 供 `*.done` 类收尾事件携带完整内容。
    pub delta_acc: std::collections::HashMap<usize, String>,
    /// MessageStart 携带的响应 id（收尾事件回填，避免发空串）。
    pub message_id: String,
    /// Core 块索引 → 客户端 tool_calls 序数。OpenAI Chat 的
    /// `tool_calls[].index` 只在工具调用内计数（0,1,2…），直接用 Core
    /// 块索引会因前置文本块产生稀疏序号，严格客户端累积出带空洞数组。
    pub tool_ordinals: std::collections::HashMap<usize, usize>,
    /// 下一个待分配的 tool_calls 序数。
    pub next_tool_ordinal: usize,
}

impl StreamEncodeState {
    /// 合并一条 MessageDelta 携带的用量（各字段取最大值）。
    pub fn merge_usage(&mut self, u: &Usage) {
        self.usage_acc.input_tokens = self.usage_acc.input_tokens.max(u.input_tokens);
        self.usage_acc.output_tokens = self.usage_acc.output_tokens.max(u.output_tokens);
        self.usage_acc.cache_read_tokens =
            self.usage_acc.cache_read_tokens.max(u.cache_read_tokens);
        self.usage_acc.cache_write_tokens =
            self.usage_acc.cache_write_tokens.max(u.cache_write_tokens);
        self.usage_acc.reasoning_tokens =
            self.usage_acc.reasoning_tokens.max(u.reasoning_tokens);
    }

    /// 记录 BlockStart 的起始块。
    pub fn note_start(&mut self, index: usize, block: &moonbridge_core::ContentBlock) {
        self.blocks.insert(index, block.clone());
    }

    /// 追加块的增量载荷（文本/JSON 参数）。
    pub fn push_delta(&mut self, index: usize, text: &str) {
        self.delta_acc.entry(index).or_default().push_str(text);
    }

    /// 取 `index` 块对应的 tool_calls 序数，未分配时递增分配。
    pub fn tool_ordinal(&mut self, index: usize) -> usize {
        if let Some(&o) = self.tool_ordinals.get(&index) {
            return o;
        }
        let o = self.next_tool_ordinal;
        self.next_tool_ordinal += 1;
        self.tool_ordinals.insert(index, o);
        o
    }
}

/// decode 的每流状态。adapter 本体为并发共享单例，可变状态由 gateway
/// 按请求持有，decode 保持可重入。
///
/// 无显式块边界事件的协议（Gemini：每个 chunk 只有一串 parts）需要跨 chunk
/// 维持稳定的块索引分配与块内容累积——函数调用计数、text/reasoning 各自的
/// 索引槽位都挂在这里；带显式 `index`/`output_index` 的协议（Anthropic/
/// OpenAI）完全不用它。
#[derive(Default)]
pub struct StreamDecodeState {
    /// 命名槽位 → 块索引（键由 adapter 自定，如 "text"/"reasoning"）。
    pub block_indexes: std::collections::HashMap<String, usize>,
    /// 下一个待分配块索引（函数调用等匿名块直接取它并自增）。
    pub next_block_index: usize,
    /// 按块索引累积中的内容块（供收尾事件携带完整块，如 reasoning item
    /// 需要完整 summary + signature 还原）。
    pub blocks: std::collections::HashMap<usize, moonbridge_core::ContentBlock>,
    /// 已开启但尚未 BlockStop 的块索引（按开启顺序）。上游流正常终止
    /// （finish_reason/[DONE]）时按序补发收尾，下游客户端的块状态机
    /// 才不会留下永久开启的块。
    pub open_blocks: Vec<usize>,
    /// MessageStart 是否已发出（无显式起始事件的协议去重用）。
    pub message_started: bool,
    /// 逐块累积的 tool_call 参数 JSON 增量（上游按 delta 下发时）。
    pub args_acc: std::collections::HashMap<usize, String>,
}

impl StreamDecodeState {
    /// 取命名槽位的块索引，未分配时分配新索引并返回 `newly = true`。
    pub fn slot(&mut self, key: &str) -> (usize, bool) {
        if let Some(&i) = self.block_indexes.get(key) {
            return (i, false);
        }
        let i = self.next_block_index;
        self.next_block_index += 1;
        self.block_indexes.insert(key.to_string(), i);
        (i, true)
    }

    /// 向 `index` 处的累积块追加明文/凭据（text 拼进 Text，reasoning 同理）。
    pub fn push_text(&mut self, index: usize, reasoning: bool, text: &str) {
        let block = self.blocks.entry(index).or_insert_with(|| {
            if reasoning {
                moonbridge_core::ContentBlock::Reasoning {
                    text: String::new(),
                    signature: None,
                    redacted: false,
                }
            } else {
                moonbridge_core::ContentBlock::text(String::new())
            }
        });
        match block {
            moonbridge_core::ContentBlock::Text { text: t }
            | moonbridge_core::ContentBlock::Reasoning { text: t, .. } => t.push_str(text),
            _ => {}
        }
    }

    /// 记录 `index` 处累积块的推理凭据。
    pub fn push_signature(&mut self, index: usize, signature: &str) {
        let block = self.blocks.entry(index).or_insert_with(|| {
            moonbridge_core::ContentBlock::Reasoning {
                text: String::new(),
                signature: None,
                redacted: false,
            }
        });
        if let moonbridge_core::ContentBlock::Reasoning { signature: s, .. } = block {
            *s = Some(signature.to_string());
        }
    }

    /// 登记一个开启的块（重复登记幂等）。
    pub fn open_block(&mut self, index: usize) {
        if !self.open_blocks.contains(&index) {
            self.open_blocks.push(index);
        }
    }

    /// 关闭一个块（返回是否原本开启）。
    pub fn close_block(&mut self, index: usize) -> bool {
        if let Some(pos) = self.open_blocks.iter().position(|&i| i == index) {
            self.open_blocks.remove(pos);
            true
        } else {
            false
        }
    }
}

/// Core ↔ 上游协议（非流式）。例：CoreRequest → Anthropic Messages 请求。
#[async_trait]
pub trait ProviderAdapter: Send + Sync {
    fn protocol(&self) -> Protocol;
    /// CoreRequest → 上游待发送请求（含 URL/headers/body）。
    ///
    /// 命名口径同上（转换方向，非构造函数；`&self` 为动态派发所必需）。
    #[allow(clippy::wrong_self_convention)]
    async fn from_core_request(
        &self,
        ctx: &ReqCtx,
        req: &CoreRequest,
        endpoint: &ProviderEndpoint,
    ) -> Result<UpstreamRequest>;
    /// 上游响应 JSON → CoreResponse。
    async fn to_core_response(&self, ctx: &ReqCtx, raw: Value) -> Result<CoreResponse>;
}

/// 上游 SSE → Core 流事件（流式）。
#[async_trait]
pub trait ProviderStreamAdapter: Send + Sync {
    fn protocol(&self) -> Protocol;
    /// 把一个上游 SSE chunk 解码为 0..n 个 Core 流事件。
    ///
    /// gateway 已在解码前对该 chunk 触发 `on_upstream_chunk_raw` 钩子。
    /// `st` 为每流状态（gateway 每个流式请求持有一份）：无显式块边界的
    /// 协议（Gemini）靠它跨 chunk 稳定分配块索引、累积块内容。
    fn decode(
        &self,
        ctx: &ReqCtx,
        st: &mut StreamDecodeState,
        chunk: &RawChunk,
    ) -> Result<Vec<CoreStreamEvent>>;
}
