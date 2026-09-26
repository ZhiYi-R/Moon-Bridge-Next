//! Rust（Core IR / Raw 报文）↔ Lua table 转换。
//!
//! 关键约定：`RawMessage.body` / `RawChunk.data` 在 Lua 侧直接呈现为 table（JSON）
//! 或 string（文本），而非 Rust 内部的 tagged enum，兑现「Lua 直接操作请求体本身」。
//! 利用 Lua table 的引用语义：宿主把报文构造成 table 传入钩子，插件就地修改后，
//! 宿主从同一 table 回写；返回值仅用于表达 short_circuit / abort / drop 等动作。

use mlua::{Lua, LuaSerdeExt, Table, Value as LuaValue};
use serde::Serialize;
use serde_json::Value;

use moonbridge_core::Protocol;
use moonbridge_protocol::{
    ChunkStage, ChunkVerdict, RawBody, RawChunk, RawMessage, RawStage, RawVerdict, ReqCtx,
};

use crate::error::Result;

/// 传给每个钩子的上下文 table（只读）。
#[derive(Serialize)]
pub struct LuaCtx<'a> {
    pub request_id: &'a str,
    pub session_id: Option<&'a str>,
    pub model_alias: &'a str,
    pub client_protocol: &'a str,
    pub upstream_protocol: Option<&'a str>,
    pub provider: Option<&'a str>,
    pub stream: bool,
}

impl<'a> From<&'a ReqCtx> for LuaCtx<'a> {
    fn from(c: &'a ReqCtx) -> Self {
        LuaCtx {
            request_id: &c.request_id,
            session_id: c.session_id.as_deref(),
            model_alias: &c.model_alias,
            client_protocol: c.client_protocol.as_str(),
            upstream_protocol: c.upstream_protocol.map(|p| p.as_str()),
            provider: c.provider_key.as_deref(),
            stream: c.stream,
        }
    }
}

pub fn ctx_to_lua(lua: &Lua, ctx: &ReqCtx) -> Result<LuaValue> {
    Ok(lua.to_value(&LuaCtx::from(ctx))?)
}

fn stage_str(s: RawStage) -> &'static str {
    match s {
        RawStage::ClientRequest => "client_request",
        RawStage::UpstreamRequest => "upstream_request",
        RawStage::UpstreamResponse => "upstream_response",
        RawStage::ClientResponse => "client_response",
    }
}

fn chunk_stage_str(s: ChunkStage) -> &'static str {
    match s {
        ChunkStage::UpstreamChunk => "upstream_chunk",
        ChunkStage::ClientChunk => "client_chunk",
    }
}

/// RawBody → Lua：Json→table、Text/Binary→string、Empty→nil。
pub fn body_to_lua(lua: &Lua, body: &RawBody) -> Result<LuaValue> {
    Ok(match body {
        RawBody::Json { value } => lua.to_value(value)?,
        RawBody::Text { text } => LuaValue::String(lua.create_string(text)?),
        RawBody::Binary { data } => {
            use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
            LuaValue::String(lua.create_string(B64.encode(data))?)
        }
        RawBody::Empty => LuaValue::Nil,
    })
}

/// Lua → RawBody：table→Json、string→Text、nil→Empty。
pub fn lua_to_body(lua: &Lua, v: LuaValue) -> Result<RawBody> {
    Ok(match v {
        LuaValue::Nil => RawBody::Empty,
        LuaValue::String(s) => RawBody::Text {
            text: s.to_str()?.to_string(),
        },
        LuaValue::Table(_) => RawBody::Json {
            value: lua.from_value::<Value>(v)?,
        },
        other => RawBody::Json {
            value: lua.from_value::<Value>(other)?,
        },
    })
}

/// 估算 JSON 值的序列化体积（字节）。O(n) 遍历但不做大字符串分配，
/// 用于在展开为 Lua table 前廉价判定是否超过降级阈值。
fn json_size(v: &Value) -> usize {
    match v {
        Value::Null => 4,
        Value::Bool(b) => usize::from(!*b) + 4,
        Value::Number(n) => n.to_string().len(),
        Value::String(s) => s.len() + 2,
        Value::Array(a) => a.iter().map(json_size).sum::<usize>() + a.len() + 2,
        Value::Object(m) => {
            m.iter()
                .map(|(k, v)| k.len() + 3 + json_size(v))
                .sum::<usize>()
                + 2
        }
    }
}

/// raw body 是否超过降级阈值（超过则不展开为 Lua table）。
fn body_exceeds(body: &RawBody, limit: usize) -> bool {
    match body {
        RawBody::Json { value } => json_size(value) > limit,
        RawBody::Text { text } => text.len() > limit,
        RawBody::Binary { data } => data.len() > limit,
        RawBody::Empty => false,
    }
}

/// `max_body_bytes`：body 降级阈值。超过时不展开为 Lua table（置 `body=nil` 并标记
/// `body_truncated=true`），避免过大的报文耗尽沙箱内存；回写时据此保留原始报文。
pub fn raw_message_to_lua(lua: &Lua, m: &RawMessage, max_body_bytes: usize) -> Result<Table> {
    let t = lua.create_table()?;
    t.set("stage", stage_str(m.stage))?;
    t.set("protocol", m.protocol.as_str())?;
    t.set("provider", m.provider.clone())?;
    t.set("method", m.method.clone())?;
    t.set("url", m.url.clone())?;
    t.set("status", m.status)?;
    t.set("headers", lua.to_value(&m.headers)?)?;
    t.set("session_id", m.session_id.clone())?;
    if body_exceeds(&m.body, max_body_bytes) {
        t.set("body", LuaValue::Nil)?;
        t.set("body_truncated", true)?;
    } else {
        t.set("body", body_to_lua(lua, &m.body)?)?;
    }
    Ok(t)
}

/// 把 Lua table 中插件的就地修改回写到 RawMessage（headers/body/url/status/method）。
pub fn apply_lua_to_message(lua: &Lua, t: &Table, m: &mut RawMessage) -> Result<()> {
    if let Ok(h) = t.get::<LuaValue>("headers") {
        if !matches!(h, LuaValue::Nil) {
            if let Ok(headers) = lua.from_value::<Vec<(String, String)>>(h) {
                m.headers = headers;
            }
        }
    }
    // body 降级且插件未替换（仍为 nil）时，保留原始报文，避免截断污染真实请求/响应
    let truncated = t.get::<Option<bool>>("body_truncated")?.unwrap_or(false);
    let body_v = t.get::<LuaValue>("body")?;
    if !(truncated && matches!(body_v, LuaValue::Nil)) {
        m.body = lua_to_body(lua, body_v)?;
    }
    if let Ok(u) = t.get::<Option<String>>("url") {
        m.url = u;
    }
    if let Ok(s) = t.get::<Option<u16>>("status") {
        m.status = s;
    }
    if let Ok(meth) = t.get::<Option<String>>("method") {
        m.method = meth;
    }
    if let Ok(s) = t.get::<Option<String>>("session_id") {
        m.session_id = s;
    }
    Ok(())
}

/// RawChunk → Lua table。`max_body_bytes` 语义同 [`raw_message_to_lua`]（字段为 `data`）。
pub fn raw_chunk_to_lua(lua: &Lua, c: &RawChunk, max_body_bytes: usize) -> Result<Table> {
    let t = lua.create_table()?;
    t.set("stage", chunk_stage_str(c.stage))?;
    t.set("protocol", c.protocol.as_str())?;
    t.set("provider", c.provider.clone())?;
    t.set("event", c.event.clone())?;
    if body_exceeds(&c.data, max_body_bytes) {
        t.set("data", LuaValue::Nil)?;
        t.set("data_truncated", true)?;
    } else {
        t.set("data", body_to_lua(lua, &c.data)?)?;
    }
    t.set("raw", c.raw.clone())?;
    Ok(t)
}

/// 把 Lua table 中插件的就地修改回写到 RawChunk（event/data/raw）。
pub fn apply_lua_to_chunk(lua: &Lua, t: &Table, c: &mut RawChunk) -> Result<()> {
    if let Ok(e) = t.get::<Option<String>>("event") {
        c.event = e;
    }
    // data 降级且插件未替换时保留原始 data
    let truncated = t.get::<Option<bool>>("data_truncated")?.unwrap_or(false);
    let data_v = t.get::<LuaValue>("data")?;
    if !(truncated && matches!(data_v, LuaValue::Nil)) {
        c.data = lua_to_body(lua, data_v)?;
    }
    // `ok().flatten()`：读取失败（Err）与字段缺失（None）同样按「未改写 raw」处理
    if let Some(raw) = t.get::<Option<String>>("raw").ok().flatten() {
        c.raw = raw;
    }
    Ok(())
}

/// 解析报文钩子返回值为动作判定（`None` 表示 Pass，修改已通过 table 回写）。
pub fn parse_raw_action(lua: &Lua, ret: LuaValue) -> Result<Option<RawVerdict>> {
    let LuaValue::Table(t) = ret else {
        return Ok(None);
    };
    let Ok(action) = t.get::<Option<String>>("action") else {
        return Ok(None);
    };
    let Some(action) = action else {
        return Ok(None);
    };
    match action.as_str() {
        "short_circuit" => {
            let status = t.get::<Option<u16>>("status")?.unwrap_or(200);
            let headers = match t.get::<LuaValue>("headers")? {
                LuaValue::Nil => Vec::new(),
                h => lua.from_value::<Vec<(String, String)>>(h)?,
            };
            let body = lua_to_body(lua, t.get::<LuaValue>("body")?)?;
            Ok(Some(RawVerdict::ShortCircuit {
                status,
                headers,
                body,
            }))
        }
        "abort" => {
            let message = t
                .get::<Option<String>>("message")?
                .unwrap_or_else(|| "aborted by plugin".to_string());
            Ok(Some(RawVerdict::Abort { message }))
        }
        // 重试当前上游请求（仅上游响应钩子的错误路径消费；见 dispatch）：delay_ms 缺省 0，
        // 非法类型（字符串/表等）与其它分支一样按错误处理，由上层降级为放行。
        "retry" => {
            let delay_ms = t.get::<Option<u64>>("delay_ms")?.unwrap_or(0);
            Ok(Some(RawVerdict::Retry { delay_ms }))
        }
        _ => Ok(None), // "pass" 或未知动作均视为放行
    }
}

/// 解析流式 chunk 钩子返回值（`{action="drop"}` → Drop）。
pub fn parse_chunk_action(ret: LuaValue) -> ChunkVerdict {
    if let LuaValue::Table(t) = ret {
        if let Ok(Some(action)) = t.get::<Option<String>>("action") {
            if action == "drop" {
                return ChunkVerdict::Drop;
            }
        }
    }
    ChunkVerdict::Forward
}

pub fn protocol_str(p: Protocol) -> &'static str {
    p.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造 `{ action = "retry" }` 形态的返回值（可选 `delay_ms`）。
    fn retry_table(lua: &Lua, delay: LuaValue) -> LuaValue {
        let t = lua.create_table().unwrap();
        t.set("action", "retry").unwrap();
        if !matches!(delay, LuaValue::Nil) {
            t.set("delay_ms", delay).unwrap();
        }
        LuaValue::Table(t)
    }

    /// `{action="retry"}`：缺省延迟为 0（由宿主立即重发，延迟钳制仍受配置约束）。
    #[test]
    fn parses_retry_action_with_default_delay() {
        let lua = Lua::new();
        let v = retry_table(&lua, LuaValue::Nil);
        assert_eq!(
            parse_raw_action(&lua, v).unwrap(),
            Some(RawVerdict::Retry { delay_ms: 0 })
        );
    }

    /// `delay_ms` 显式给出时原样采用（rescue_5xx 的指数退避完全靠它表达）。
    #[test]
    fn parses_retry_action_with_explicit_delay() {
        let lua = Lua::new();
        let v = retry_table(&lua, LuaValue::Integer(700));
        assert_eq!(
            parse_raw_action(&lua, v).unwrap(),
            Some(RawVerdict::Retry { delay_ms: 700 })
        );
    }

    /// 不可转换的 delay_ms 类型必须报错（上层降级为放行），不能静默当 0 处理——否则
    /// 插件写错类型就成了「无延迟猛重试」，与硬防护的意图正好相反。
    ///
    /// 注：Lua 自身的字符串→数字强制转换在这一层仍然生效（`delay_ms = "700"` 解成
    /// 700），这是 mlua 与 `status` 等既有分支共有的口径，不在本分支单独收窄。
    #[test]
    fn rejects_unconvertible_delay_type() {
        let lua = Lua::new();
        let t = lua.create_table().unwrap();
        let v = retry_table(&lua, LuaValue::Table(t));
        assert!(
            parse_raw_action(&lua, v).is_err(),
            "表类型 delay_ms 应报解码错误"
        );

        let v2 = retry_table(&lua, LuaValue::String(lua.create_string("700").unwrap()));
        assert_eq!(
            parse_raw_action(&lua, v2).unwrap(),
            Some(RawVerdict::Retry { delay_ms: 700 }),
            "数字字符串按 Lua 语义强制转换"
        );
    }

    /// 既有口径不变：`pass` 与未知动作仍视为放行（None），非 table 返回值同样放行。
    #[test]
    fn unknown_action_and_non_table_pass() {
        let lua = Lua::new();
        let t = lua.create_table().unwrap();
        t.set("action", "pass").unwrap();
        assert_eq!(parse_raw_action(&lua, LuaValue::Table(t)).unwrap(), None);

        let t2 = lua.create_table().unwrap();
        t2.set("action", "future_action").unwrap();
        assert_eq!(parse_raw_action(&lua, LuaValue::Table(t2)).unwrap(), None);

        assert_eq!(parse_raw_action(&lua, LuaValue::Nil).unwrap(), None);
    }
}
