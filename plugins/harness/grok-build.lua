-- harness/grok-build：剥离 grok CLI（xAI GrokBuild）的会话头。
--
-- grok 的 GrokRequestHeaders 对所有 LLM 端点（含第三方 OpenAI 兼容提供方）
-- 都发送一组 x-grok-* 头，其中会话粒度两级：
--   x-grok-session-id  CLI 会话（最稳定的亲和粒度）
--   x-grok-conv-id     会话内单个对话
-- x-grok-req-id 是请求级、x-grok-user-id 是安装级，均不作身份源。
-- 命中任一即写入 `msg.session_id` 并把两个会话头一并剥除。

MB = {
  version = "0.1.0",
  scopes = { "global" },
  capabilities = { "raw_request" },
}

local SESSION_HEADERS = { "x-grok-session-id", "x-grok-conv-id" }

function MB.on_client_request_raw(ctx, msg)
  for _, name in ipairs(SESSION_HEADERS) do
    local s = mb.headers.get(msg.headers, name)
    if s and s ~= "" then
      msg.session_id = s
      break
    end
  end
  if msg.session_id then
    for _, name in ipairs(SESSION_HEADERS) do
      msg.headers = mb.headers.remove(msg.headers, name)
    end
  end
end
