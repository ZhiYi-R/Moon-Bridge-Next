-- harness/codex：剥离 codex 客户端的会话头。
--
-- codex 发三级会话粒度头（codex-rs/core/client.rs、codex-api/requests/headers.rs）：
--   session-id         整个 codex 进程会话（最稳定的亲和粒度）
--   thread-id          会话内的会话线程
--   x-codex-window-id  线程内的窗口（"thread-id:generation"）
-- 宿主 extract_session 已原生识别 x-codex-window-id 与 body 的
-- session_id / previous_response_id，本插件把三级头一并纳入身份通道
-- 并剥除原头，优先级取最粗粒度（session-id 亲和覆盖整会话）。
--
-- 没有会话头的请求直接放行，回到水印 marker / 新分配的回退路径。

MB = {
  version = "0.2.0",
  scopes = { "global" },
  capabilities = { "raw_request" },
}

local SESSION_HEADERS = { "session-id", "thread-id", "x-codex-window-id" }

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
