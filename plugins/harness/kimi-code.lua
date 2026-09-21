-- harness/kimi-code：从 Kimi Code CLI 的缓存亲和字段提取会话身份。
--
-- Kimi Code 不发会话头（X-Msh-* 设备头只对 kimi 一方提供方下发），会话
-- 标识在请求体里，按协议二选一，值都是 session id：
--   prompt_cache_key    kimi / openai / openai_responses 协议
--   metadata.user_id    anthropic 协议
-- 两个字段本身就是给上游的缓存亲和语义，照常转发——只取身份，不剥报文。

MB = {
  version = "0.1.0",
  scopes = { "global" },
  capabilities = { "raw_request" },
}

function MB.on_client_request_raw(ctx, msg)
  -- JSON 报文在 Lua 侧已呈现为 table；非 JSON / 超阈值降级时 body 为 nil 或 string
  if type(msg.body) ~= "table" then return end
  local s = msg.body.prompt_cache_key
  if type(s) ~= "string" or s == "" then
    local meta = msg.body.metadata
    if type(meta) == "table" then s = meta.user_id end
  end
  if type(s) == "string" and s ~= "" then msg.session_id = s end
end
