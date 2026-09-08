-- Moon Bridge Next 示例插件：Core IR 语义层（capabilities = {"core"}）
--
-- 演示如何在「协议中立」的 Core IR 上操作请求/响应：
--   * on_request      —— 打印请求概要、会话级计数、注入一条 system 提示
--   * on_response     —— 记录用量
--   * transform_error —— 给错误消息加前缀
--
-- 注册约定：脚本执行后暴露全局 `MB` 表，既承载清单（version/scopes/capabilities/
-- config_schema），也承载钩子函数。宿主 API 挂在全局 `mb`（小写）下。

MB = {
  version = "0.1.0",
  scopes = { "global" },
  capabilities = { "core" },
  -- 供 UI 渲染配置表单（可选）
  config_schema = {
    type = "object",
    properties = {
      reminder = { type = "string", title = "注入到 system 的提示语" },
    },
  },
}

function MB.init()
  mb.log.info("log_request 插件已初始化")
end

-- ctx: {request_id, session_id, model_alias, client_protocol, upstream_protocol, provider, stream}
-- req: CoreRequest（就地修改即可生效；亦可 return 一个新 table）
function MB.on_request(ctx, req)
  mb.log.info(string.format(
    "[on_request] model=%s alias=%s stream=%s messages=%d",
    req.model or "-", req.model_alias or "-", tostring(ctx.stream), #req.messages))

  -- 会话级请求计数：mb.session 按「插件名 + session_id」隔离
  local n = (mb.session.get("count") or 0) + 1
  mb.session.set("count", n)

  -- 注入一条 system 提示（ContentBlock 文本块形如 {type="text", text=...}）
  local reminder = (mb.config and mb.config.reminder)
    or "You are served via Moon Bridge Next local gateway."
  table.insert(req.system, 1, { type = "text", text = reminder })

  if n > 1 then
    mb.log.debug(string.format("[on_request] 本会话第 %d 次请求", n))
  end
end

-- resp: CoreResponse {id, model, content, stop_reason, usage, ext}
function MB.on_response(ctx, resp)
  local u = resp.usage or {}
  mb.log.info(string.format(
    "[on_response] id=%s in=%s out=%s cache_read=%s",
    resp.id or "-", tostring(u.input_tokens), tostring(u.output_tokens),
    tostring(u.cache_read_tokens)))
end

-- 转换错误消息（返回字符串）
function MB.transform_error(ctx, msg)
  return "[moonbridge] " .. (msg or "unknown error")
end
