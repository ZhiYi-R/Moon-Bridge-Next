-- Moon Bridge Next 示例插件：出入站原始报文层
-- capabilities = {"raw_request", "raw_stream"}
--
-- 与 Core IR 层不同，报文层钩子直接读写「原始 HTTP 报文」：headers（保序数组）与
-- body（JSON 自动 ↔ Lua table，非 JSON 为 string）。位于协议转换的最外层，可改写、
-- 短路（本地直接应答）、中止（返回错误），或丢弃单个 SSE chunk。
--
--   * on_client_request_raw   入站请求（客户端 → 网关）      —— 需 raw_request
--   * on_upstream_request_raw 出站请求（网关 → 上游）        —— 需 raw_request
--   * on_upstream_chunk_raw   上游 SSE chunk（流式）         —— 需 raw_stream
--
-- msg   = {stage, protocol, provider, method, url, status, headers={{k,v},...}, body}
-- chunk = {stage, protocol, provider, event, data, raw}
-- 就地修改 headers/body/url 即生效；返回值仅表达动作（nil 表示放行）。

MB = {
  version = "0.1.0",
  scopes = { "global", "provider" },
  capabilities = { "raw_request", "raw_stream" },
}

-- 入站请求：演示鉴权检查（默认仅告警，取消注释即强制拦截）。
function MB.on_client_request_raw(ctx, msg)
  local auth = mb.headers.get(msg.headers, "authorization")
  if not auth then
    mb.log.warn(string.format("[client_request] %s 缺少 Authorization 头", msg.url or "?"))
    -- 强制拦截：直接返回错误，不进入协议转换与上游调用
    -- return { action = "abort", message = "missing authorization header" }
  end
  -- 亦可本地直接应答（跳过上游）：
  -- return {
  --   action = "short_circuit",
  --   status = 200,
  --   headers = { { "content-type", "application/json" } },
  --   body = { ok = true, cached = true },
  -- }
end

-- 出站请求：改写上游头 + 给 body 打补丁。
function MB.on_upstream_request_raw(ctx, msg)
  mb.log.info(string.format("[upstream_request] %s %s", msg.method or "?", msg.url or "?"))

  -- mb.headers.set 大小写不敏感、保序；headers 是 {{name,value},...}
  msg.headers = mb.headers.set(msg.headers, "anthropic-version", "2023-06-01")
  msg.headers = mb.headers.set(msg.headers, "x-moonbridge-session", ctx.session_id or "anon")

  -- body 为 JSON table 时可直接读写（例如注入 metadata、覆写参数）
  if type(msg.body) == "table" then
    msg.body.metadata = {
      source = "moonbridge-next",
      provider = ctx.provider or "unknown",
    }
    -- 例：强制上限，防止超大 max_tokens 透传到上游
    if msg.body.max_tokens and msg.body.max_tokens > 8192 then
      mb.log.warn("[upstream_request] max_tokens 超过 8192，已收敛")
      msg.body.max_tokens = 8192
    end
  end
end

-- 上游 SSE chunk：丢弃心跳、按需改写事件。
function MB.on_upstream_chunk_raw(ctx, chunk)
  -- 丢弃上游 ping/心跳事件，减少无谓转发
  if chunk.event == "ping" then
    return { action = "drop" }
  end
  -- data 为 JSON table 时可就地改写（例如给 message_start 追加标记）
  if type(chunk.data) == "table" and chunk.data.type == "message_start" then
    mb.log.info("[upstream_chunk] message_start 已放行")
  end
  -- 返回 nil：照常转发（就地修改已通过引用语义回写）
end
