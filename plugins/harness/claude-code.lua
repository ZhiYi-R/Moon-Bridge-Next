-- harness/claude-code：从 Claude Code 的 metadata.user_id 提取会话身份。
--
-- Claude Code 把会话 UUID 嵌进 body.metadata.user_id，两种线上格式：
--   claude-cli 1.x 扁平串：user_<hash>_account_<uuid>_session_<uuid>
--   claude-cli 2.x JSON： {"device_id":..,"account_uuid":..,"session_id":..}
-- 取其中的 session UUID 写入 `msg.session_id`，即可在无水印回环的情况下
-- 稳定亲和上游缓存。
--
-- metadata 本身属于请求语义照常转发，仅提取身份不改报文。

MB = {
  version = "0.2.0",
  scopes = { "global" },
  capabilities = { "raw_request" },
}

function MB.on_client_request_raw(ctx, msg)
  -- JSON 报文在 Lua 侧已呈现为 table；非 JSON / 超阈值降级时 body 为 nil 或 string
  if type(msg.body) ~= "table" then return end
  local meta = msg.body.metadata
  if type(meta) ~= "table" or type(meta.user_id) ~= "string" then return end
  local uid = meta.user_id
  local s
  if uid:sub(1, 1) == "{" then
    -- 2.x JSON blob：模式提取 session_id，不走完整 JSON 解码
    s = uid:match('"session_id"%s*:%s*"([^"]+)"')
  else
    -- 1.x 扁平串：session_ 后随 uuid
    s = uid:match("session_([%x][%x-]*)")
  end
  s = s or uid -- 形态不符时整体仍是稳定标识
  if s ~= "" then msg.session_id = s end
end
