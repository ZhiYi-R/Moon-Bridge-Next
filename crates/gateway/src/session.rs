//! 会话水印：把 session id 以 marker 形式附在助手**纯文本**输出末尾，靠客户端把整段
//! 对话历史原样带回，从而在下一轮被识别回同一会话。
//!
//! 为什么需要它：Qwen Code 这类客户端在请求头与请求体里都不携带会话标识（其
//! `prompt_cache_key` 注入路径以 hostname 恰为 `api.openai.com` 为前提，经网关时不生效），
//! 于是 `ctx.session_id` 恒为 `None`，插件的 `mb.session` 与 trace 的会话目录都失去粒度。
//!
//! 关键设计：**入站即剥除，剥完才转发上游**。上游模型的输入里永远不出现 marker，因此
//! 模型没有模仿它的机会（模仿只会发生在 marker 进入上下文时），marker 的唯一存活期是
//! 客户端本地 transcript。这也意味着发往上游的 prompt 比客户端存档更干净。
//!
//! marker 形态：`[mb:xxxxxx]`，6 位小写十六进制短 tag。短 tag 不是 session id 本身，
//! 而是 [`SessionTable`] 里指向完整 uuid 的键——这样 marker 足够省 token，同时内部
//! 仍用 uuid 做主键。短 tag 不在活跃表内（被淘汰 / 网关重启）即按新会话处理。

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use moonbridge_core::{ContentBlock, CoreRequest, CoreResponse, CoreStreamEvent, StreamDelta};

const OPEN: &str = "[mb:";
const CLOSE: char = ']';
/// 短 tag 长度（6 位 hex = 24 bit；活跃表深度上限内碰撞概率可忽略）。
const TAG_LEN: usize = 6;

/// 追加到文本末尾的 marker 片段（带一个前置空格，便于与正文分隔）。
pub fn marker_inline(tag: &str) -> String {
    format!("{OPEN}{tag}{CLOSE}")
}

fn valid_tag(s: &str) -> bool {
    s.len() == TAG_LEN && s.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// 由任意 id 派生短 tag（FNV-1a 取低 `TAG_LEN` 个 hex 位，**定长**且稳定）。
///
/// 不能只挑 id 里现成的 hex 字符：外部 `session_id` 可能是任意字符串（例如
/// `"ext-sess-1"` 只剩 `e`/`e`/`1` 三个 hex），挑出来的 tag 长度不足，下一轮
/// [`valid_tag`] 不认 ⇒ marker 剥不掉、逐轮在客户端 transcript 里累积。定长哈希
/// 保证「打得上，也剥得掉」，同时保持同一 id 稳定映射同一 tag。
fn tag_from_id(id: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in id.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    let mut s = format!("{:x}", h & ((1u64 << (TAG_LEN * 4)) - 1));
    if s.len() < TAG_LEN {
        s.insert_str(0, &"0".repeat(TAG_LEN - s.len()));
    }
    s
}

/// 从文本中剥除所有合法 marker，返回清洗后的文本与**按出现顺序**收集的 tag。
fn strip_text(s: &str) -> (String, Vec<String>) {
    let mut out = String::with_capacity(s.len());
    let mut tags = Vec::new();
    let mut rest = s;
    while let Some(pos) = rest.find(OPEN) {
        out.push_str(&rest[..pos]);
        let after = &rest[pos + OPEN.len()..];
        match after.find(CLOSE) {
            Some(end) if valid_tag(&after[..end]) => {
                tags.push(after[..end].to_string());
                // 一并吃掉 marker 前紧邻的那个空格（若有），避免留下悬挂空白
                if out.ends_with(' ') {
                    out.pop();
                }
                rest = &after[end + CLOSE.len_utf8()..];
            }
            // 形似但不是我们的 marker（如正文里的 `[mb:abc]`）：原样保留，冒号后内容照抄
            _ => {
                out.push_str(OPEN);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    (out, tags)
}

/// 遍历内容块，剥除其中的 marker；被剥成空块的（原本只装了 marker）一并删掉。
///
/// 删空块是必要的：流式路径的水印独占一个文本块（见 [`marker_blocks`]），客户端带回
/// 下一轮时该块剥完就剩 `{"type":"text","text":""}`，Anthropic 会直接拒收空文本块。
/// 保底不删到 `content` 变空——那同样是上游非法输入，宁可留一个空块也不能交出空数组。
fn strip_blocks(blocks: &mut Vec<ContentBlock>, found: &mut Vec<String>) {
    let mut emptied = Vec::new();
    for (idx, b) in blocks.iter_mut().enumerate() {
        match b {
            ContentBlock::Text { text } => {
                let (cleaned, tags) = strip_text(text);
                if tags.is_empty() {
                    continue;
                }
                *text = cleaned;
                found.extend(tags);
                if text.is_empty() {
                    emptied.push(idx);
                }
            }
            ContentBlock::ToolResult { content, .. } => strip_blocks(content, found),
            _ => {}
        }
    }
    for idx in emptied.into_iter().rev() {
        if blocks.len() > 1 {
            blocks.remove(idx);
        }
    }
}

/// 剥除请求内全部 marker，返回**最后一次**出现的 tag（= 最新一轮附加的那个）。
///
/// 取最后一个而非第一个：同一会话的 marker 恒定，历史里若混入陈旧 tag（会话被淘汰后
/// 重新分配）应以最近一次为准。
pub fn extract_from_request(req: &mut CoreRequest) -> Option<String> {
    let mut found = Vec::new();
    for m in &mut req.messages {
        strip_blocks(&mut m.content, &mut found);
    }
    strip_blocks(&mut req.system, &mut found);
    found.pop()
}

/// 把 marker 追加到响应的文本输出末尾。返回是否追加成功。
///
/// 两个前提（缺一即不追加，宁缺毋滥）：
/// 1. 不含 `ToolUse` 块 —— 用户要求「非 tool 调用输出文本」，且工具循环轮次无需打标；
/// 2. 至少有一个 `Text` 块 —— 绝不凭空造一个文本块，那会改变响应的块结构。
pub fn append_to_response(resp: &mut CoreResponse, tag: &str) -> bool {
    if resp.content.iter().any(|b| matches!(b, ContentBlock::ToolUse { .. })) {
        return false;
    }
    match resp
        .content
        .iter_mut()
        .rev()
        .find_map(|b| match b {
            ContentBlock::Text { text } => Some(text),
            _ => None,
        }) {
        Some(text) => {
            // 与正文隔一个空白；[`strip_text`] 会连带吃掉这一个空格，故剥除后字节还原。
            if !text.is_empty() && !text.ends_with(char::is_whitespace) {
                text.push(' ');
            }
            text.push_str(&marker_inline(tag));
            true
        }
        None => false,
    }
}

/// 流式路径：一段**独立文本块**形式的 marker（start → delta → stop）。
///
/// 为什么不并进已有的文本块：那要求把该块的 `BlockStop` 扣住、等确认整轮无 `tool_use`
/// 再补发，等于在流中间重放事件、极易与各入口编码器的块状态机打架。另起一个新块在
/// 四种入口协议上都是合法结构，代价只是 transcript 里多一个块——反正入站会连块带字
/// 一起剥净。
pub fn marker_blocks(tag: &str, index: usize) -> Vec<CoreStreamEvent> {
    vec![
        CoreStreamEvent::BlockStart {
            index,
            block: ContentBlock::text(String::new()),
        },
        CoreStreamEvent::BlockDelta {
            index,
            delta: StreamDelta::Text {
                text: marker_inline(tag),
            },
        },
        CoreStreamEvent::BlockStop { index },
    ]
}

/// 活跃会话表：tag → session uuid，按登记顺序 FIFO 淘汰，深度可配。
///
/// 淘汰即代表该会话「不再活跃」：其 tag 从表里摘除（客户端再带回即视为新会话），
/// 同时把 uuid 反馈给调用方去清理插件侧的会话状态。
pub struct SessionTable {
    depth: usize,
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    order: VecDeque<String>,
    by_tag: HashMap<String, String>,
}

impl SessionTable {
    /// 以给定深度构造（`depth < 1` 时按 1 处理，避免表恒空导致会话永远无法复用）。
    pub fn new(depth: usize) -> Self {
        Self {
            depth: depth.max(1),
            inner: Mutex::new(Inner::default()),
        }
    }

    /// 用已有 session id 占位（供 `session_id` 字段 / `X-Codex-Window-Id` 等外部来源复用）。
    pub fn note_external(&self, session_id: &str) -> (String, Vec<String>) {
        let tag = tag_from_id(session_id);
        self.register(tag, session_id.to_string())
    }

    /// 新分配一个会话。返回 `(session_id, 被挤出的 session id 列表, 本次生效的 tag)`。
    pub fn new_session(&self) -> (String, Vec<String>, String) {
        let id = uuid::Uuid::new_v4().to_string();
        let tag = tag_from_id(&id);
        let (tag, evicted) = self.register(tag, id.clone());
        (id, evicted, tag)
    }

    /// 按 tag 查活跃会话。
    pub fn lookup(&self, tag: &str) -> Option<String> {
        self.inner.lock().ok()?.by_tag.get(tag).cloned()
    }

    /// 当前活跃会话数（测试与可观测性用）。
    pub fn len(&self) -> usize {
        self.inner.lock().map(|i| i.order.len()).unwrap_or(0)
    }

    /// 表是否为空。
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 登记 `tag → id`，超深则按 FIFO 挤出最老的，返回**实际生效的 tag** 与被挤出的 session id。
    fn register(&self, tag: String, id: String) -> (String, Vec<String>) {
        let Ok(mut g) = self.inner.lock() else {
            return (tag, Vec::new());
        };
        if let Some(prev) = g.by_tag.get(&tag) {
            if prev == &id {
                return (tag.clone(), Vec::new()); // 已登记，幂等
            }
        }
        // tag 撞车（不同 id 抢同一 tag）：重新摇一个，别把已有会话顶掉。
        // 先按 id+序号确定性重摇（可复现），连撞 32 次才退化到随机 uuid——表深远小于
        // tag 空间（16.7M），这条路实际走不到，只是保证不会静默覆盖别人的会话。
        let mut tag = tag;
        let mut tries = 0u32;
        while g.by_tag.contains_key(&tag) {
            tries += 1;
            tag = if tries <= 32 {
                tag_from_id(&format!("{id}\u{1f}{tries}"))
            } else {
                tag_from_id(&uuid::Uuid::new_v4().to_string())
            };
        }
        g.by_tag.insert(tag.clone(), id);
        g.order.push_back(tag.clone());
        let mut evicted = Vec::new();
        while g.order.len() > self.depth {
            if let Some(old) = g.order.pop_front() {
                if let Some(gone) = g.by_tag.remove(&old) {
                    evicted.push(gone);
                }
            }
        }
        (tag, evicted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use moonbridge_core::{Message, Role};

    fn text_req(texts: &[&str]) -> CoreRequest {
        let mut req = CoreRequest::new("m");
        for t in texts {
            req.messages.push(Message::text(Role::Assistant, *t));
        }
        req
    }

    #[test]
    fn strip_removes_markers_and_returns_tags_in_order() {
        let mut req = text_req(&[&format!("hello {}", marker_inline("a1b2c3"))]);
        let tag = extract_from_request(&mut req);
        assert_eq!(tag.as_deref(), Some("a1b2c3"));
        let ContentBlock::Text { text } = &req.messages[0].content[0] else {
            panic!("应为文本块");
        };
        assert_eq!(text, "hello", "marker 与其前置空格都应剥净: {text:?}");
    }

    #[test]
    fn strip_keeps_last_tag_when_history_has_stale_ones() {
        let mut req = text_req(&[
            &format!("old {}", marker_inline("111111")),
            &format!("newer {}", marker_inline("222222")),
        ]);
        assert_eq!(extract_from_request(&mut req).as_deref(), Some("222222"));
        // 两处都要被剥净，不能只剥最后一个
        let joined: String = req
            .messages
            .iter()
            .flat_map(|m| m.content.iter())
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(!joined.contains("mb:"), "所有 marker 都该剥掉: {joined:?}");
    }

    /// 形似但非法的 bracket 文本必须原样保留，不能误伤用户/模型内容。
    #[test]
    fn lookalike_brackets_survive() {
        for case in ["[mb:abc]", "[mb:GGGGGG]", "[mb:1234567]", "[MB:123456]", "[mb:123456"] {
            let mut req = text_req(&[case]);
            assert_eq!(extract_from_request(&mut req), None, "{case} 不该被认作 marker");
            let ContentBlock::Text { text } = &req.messages[0].content[0] else {
                panic!()
            };
            assert_eq!(text, case, "{case} 应原样保留");
        }
    }

    #[test]
    fn marker_in_tool_result_is_also_stripped() {
        let mut req = CoreRequest::new("m");
        req.messages.push(Message {
            role: Role::Tool,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "t1".into(),
                content: vec![ContentBlock::text(format!(
                    "out {}",
                    marker_inline("ffee00")
                ))],
                is_error: false,
            }],
            ext: Default::default(),
        });
        assert_eq!(extract_from_request(&mut req).as_deref(), Some("ffee00"));
    }

    /// 只装了 marker 的文本块剥完必须删掉：流式水印独占一个块，留下空文本块会被
    /// Anthropic 拒收（`text` 块不得为空）。
    #[test]
    fn marker_only_block_is_dropped_after_strip() {
        let mut req = CoreRequest::new("m");
        req.messages.push(Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::text("Hello world"),
                ContentBlock::text(marker_inline("abc123")),
            ],
            ext: Default::default(),
        });
        assert_eq!(extract_from_request(&mut req).as_deref(), Some("abc123"));
        let msg = &req.messages[0];
        assert_eq!(msg.content.len(), 1, "空掉的水印块应被移除: {:?}", msg.content);
        let ContentBlock::Text { text } = &msg.content[0] else { panic!() };
        assert_eq!(text, "Hello world", "正文块不得受牵连");
    }

    /// 保底：消息只有一个块且它被剥空时也不得把 `content` 删成空数组（同样非法），
    /// 此时宁留一个空文本块。
    #[test]
    fn sole_marker_block_is_emptied_not_removed() {
        let mut req = CoreRequest::new("m");
        req.messages.push(Message {
            role: Role::Assistant,
            content: vec![ContentBlock::text(marker_inline("abc124"))],
            ext: Default::default(),
        });
        assert_eq!(extract_from_request(&mut req).as_deref(), Some("abc124"));
        assert_eq!(req.messages[0].content.len(), 1, "不得交出空 content");
    }

    #[test]
    fn append_skips_tool_calls_and_never_fabricates_blocks() {
        use moonbridge_core::CoreResponse;
        let mut with_tool = CoreResponse {
            id: "r".into(),
            model: "m".into(),
            content: vec![
                ContentBlock::text("sure"),
                ContentBlock::ToolUse {
                    id: "t".into(),
                    name: "n".into(),
                    namespace: None,
                    input: serde_json::json!({}),
                },
            ],
            stop_reason: None,
            usage: Default::default(),
            ext: Default::default(),
        };
        assert!(
            !append_to_response(&mut with_tool, "abcdef"),
            "含 tool_use 的响应不得打标"
        );

        let mut no_text = CoreResponse {
            content: vec![ContentBlock::Reasoning {
                text: "hmm".into(),
                signature: None,
            }],
            ..with_tool.clone()
        };
        assert!(
            !append_to_response(&mut no_text, "abcdef"),
            "无文本块时不得凭空造块"
        );
        assert_eq!(no_text.content.len(), 1);

        let mut plain = CoreResponse {
            content: vec![ContentBlock::text("hi")],
            ..with_tool.clone()
        };
        assert!(append_to_response(&mut plain, "abcdef"));
        let ContentBlock::Text { text } = &plain.content[0] else {
            panic!()
        };
        assert_eq!(text, &format!("hi {}", marker_inline("abcdef")), "应隔一个空白");

        // 打上再剥必须字节还原，否则每轮都会累积空白
        let mut req = text_req(&[text]);
        assert_eq!(extract_from_request(&mut req).as_deref(), Some("abcdef"));
        let ContentBlock::Text { text: back } = &req.messages[0].content[0] else {
            panic!()
        };
        assert_eq!(back, "hi", "打标→剥除应还原原文");
    }

    /// 流式 marker 是一段完整的独立块：start → delta → stop，索引由调用方给定。
    #[test]
    fn marker_blocks_are_a_well_formed_block_triple() {
        let blocks = marker_blocks("abc123", 7);
        assert_eq!(blocks.len(), 3);
        assert!(matches!(
            &blocks[0],
            CoreStreamEvent::BlockStart { index: 7, block: ContentBlock::Text { .. } }
        ));
        match &blocks[1] {
            CoreStreamEvent::BlockDelta { index, delta } => {
                assert_eq!(*index, 7);
                assert!(
                    matches!(delta, StreamDelta::Text { text } if *text == marker_inline("abc123")),
                    "增量应只含 marker: {delta:?}"
                );
            }
            other => panic!("期望 BlockDelta，得到 {other:?}"),
        }
        assert!(matches!(&blocks[2], CoreStreamEvent::BlockStop { index: 7 }));
    }

    #[test]
    fn table_evicts_oldest_beyond_depth_and_reports_victims() {
        let t = SessionTable::new(2);
        let (a, ev_a, a_tag) = t.new_session();
        let (_b, ev_b, b_tag) = t.new_session();
        assert!(ev_a.is_empty() && ev_b.is_empty(), "未超深不应淘汰");
        let (_c, ev_c, _c_tag) = t.new_session();
        assert_eq!(ev_c, vec![a], "FIFO 应挤出最老的那个");
        assert_eq!(t.len(), 2, "表深被钳在 2");
        assert!(t.lookup(&b_tag).is_some(), "b 仍在表内");
        assert!(t.lookup(&a_tag).is_none(), "a 已被淘汰");
    }

    #[test]
    fn lookup_hit_does_not_allocate_or_evict() {
        let t = SessionTable::new(3);
        let (id, _ev, tag) = t.new_session();
        assert_eq!(t.lookup(&tag).as_deref(), Some(id.as_str()));
        assert_eq!(t.len(), 1, "纯查询不得改变表大小");
    }

    #[test]
    fn tag_collision_does_not_hijack_existing_session() {
        let t = SessionTable::new(8);
        let (first_id, _e, first_tag) = t.new_session();
        // 同一 tag、不同 id 再登记：应换一个新 tag，而不是顶掉已有会话
        let (second_tag, _ev) = t.register(first_tag.clone(), uuid::Uuid::new_v4().to_string());
        assert_ne!(second_tag, first_tag, "不得顶掉已有 tag");
        assert_eq!(t.lookup(&first_tag).as_deref(), Some(first_id.as_str()));
    }

    #[test]
    fn depth_zero_is_clamped_to_one_not_dead_table() {
        let t = SessionTable::new(0);
        let (id, _ev, tag) = t.new_session();
        assert_eq!(t.lookup(&tag).as_deref(), Some(id.as_str()), "表不该恒空");
    }

    #[test]
    fn note_external_reuses_stable_tag_for_known_id() {
        let t = SessionTable::new(4);
        let id = "sess-abcdef123456";
        let (tag1, ev1) = t.note_external(id);
        let (tag2, ev2) = t.note_external(id);
        assert_eq!(tag1, tag2, "同一外部 id 应稳定映射到同一 tag");
        assert!(ev1.is_empty() && ev2.is_empty());
        assert_eq!(t.len(), 1);
    }

    /// 外部 `session_id` 未必是 uuid。旧实现只挑 id 里现成的 hex 字符，`"ext-sess-1"`
    /// 只剩 3 个 ⇒ 产出长度不足的 tag，下一轮 `valid_tag` 不认、marker 剥不掉，
    /// 于是水印逐轮在客户端 transcript 里累积。定长哈希必须保证「打得上也剥得掉」。
    #[test]
    fn external_id_with_sparse_hex_still_round_trips() {
        let t = SessionTable::new(4);
        let (tag, _ev) = t.note_external("ext-sess-1");
        assert!(valid_tag(&tag), "外部 id 派生的 tag 必须合法可剥: {tag}");

        let mut req = text_req(&[&format!("body {}", marker_inline(&tag))]);
        assert_eq!(extract_from_request(&mut req).as_deref(), Some(tag.as_str()));
        let ContentBlock::Text { text } = &req.messages[0].content[0] else {
            panic!()
        };
        assert_eq!(text, "body", "水印应被剥净、正文不受损");
        assert_eq!(t.lookup(&tag).as_deref(), Some("ext-sess-1"));
    }

    /// 不同 id 应当分散到不同 tag（哈希退化时这条会先响）。
    #[test]
    fn tag_from_id_spreads_over_distinct_ids() {
        let mut seen = std::collections::HashSet::new();
        for i in 0..512 {
            assert!(
                seen.insert(tag_from_id(&uuid::Uuid::new_v4().to_string())),
                "id #{i} 的 tag 撞车异常频繁"
            );
        }
        assert_eq!(seen.len(), 512);
    }
}
