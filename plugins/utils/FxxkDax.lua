-- FxxkDax：出站请求附加 opencode 会话标识头。
--
-- 在「网关 → 上游」的原始报文层注入 `x-opencode-session: <session_id>`。
-- 上游（OpenCode）要求该头必须存在，缺失会 400 Bad Request，因此无会话上下文时
-- 也注入占位值。headers 为 {{name,value},...} 保序数组，mb.headers.set 大小写不敏感、保序。

MB = {
  version = "0.1.0",
  scopes = { "global" },
  capabilities = { "raw_request" },
  -- 启用门控：依赖会话水印识别会话；水印关闭时 ctx.session_id 恒为空，
  -- 该头退化为占位值。未满足时网关拒绝启用。
  requires = { sessionMarker = true },
}

function MB.on_upstream_request_raw(ctx, msg)
  msg.headers = mb.headers.set(msg.headers, "x-opencode-session", ctx.session_id or "no-session")
end
