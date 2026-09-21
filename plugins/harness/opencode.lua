-- harness/opencode：剥离 opencode 客户端的会话头。
--
-- opencode 按提供方发不同的会话头（session/llm/request.ts）：
--   opencode 提供方：x-opencode-session
--   其他提供方：    x-session-affinity + x-session-id（同值）
-- 经网关时宿主不识别这些头，这里转成会话身份：写 `msg.session_id`
-- （最高优先级身份源）并把原头从入站报文剥除。
-- 另有 x-parent-session-id 仅随主会话头附带发送，不作身份源。
--
-- 没有会话头的请求直接放行，回到水印 marker / 新分配的回退路径。

MB = {
  version = "0.2.0",
  scopes = { "global" },
  capabilities = { "raw_request" },
}

local SESSION_HEADERS = { "x-opencode-session", "x-session-affinity", "x-session-id" }

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
