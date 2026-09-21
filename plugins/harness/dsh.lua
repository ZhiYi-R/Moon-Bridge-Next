-- harness/dsh：剥离 DeepSeek Harness（dsh）的会话头。
--
-- dsh 的 DeepSeekAdapter 在模型请求上携带：
--   x-deepseek-harness-session-id  会话 id（无会话上下文的请求不带）
--   x-deepseek-harness-user-id     安装级匿名 id——粒度太粗，不作身份源
--   x-deepseek-harness-compact     压缩轮次标记，与会话无关
-- body 扩展字段 dsh_session_log.session.id 是同一份会话 id，作头缺失时的
-- 回退来源（该字段属上游会话日志语义，照常转发不剥）。

MB = {
  version = "0.1.0",
  scopes = { "global" },
  capabilities = { "raw_request" },
}

local SESSION_HEADER = "x-deepseek-harness-session-id"

function MB.on_client_request_raw(ctx, msg)
  local s = mb.headers.get(msg.headers, SESSION_HEADER)
  if (not s or s == "") and type(msg.body) == "table" then
    local log = msg.body.dsh_session_log
    if type(log) == "table" and type(log.session) == "table" then
      s = log.session.id
    end
  end
  if type(s) == "string" and s ~= "" then
    msg.session_id = s
    msg.headers = mb.headers.remove(msg.headers, SESSION_HEADER)
  end
end
