--- Claude 账户认证插件（PKCE 授权码流，对齐 Claude Code）。
---
--- 令牌包契约（见 plugins/moonbridge.lua 的 MB.auth_* 段）：core 只认
--- access / expires_at（服务端原始过期时刻，unix 毫秒；skew 由 core 统一扣），
--- 其余字段（refresh / account / email / source）是本插件私有、随包透传。
---
--- 回调：redirect_uri 是 Anthropic 注册的固定值 http://localhost:54545/callback，
--- 端口被占用时无法走本流程（显式报错）。浏览器拿不到回调时可手动粘贴
--- callback URL 或 `code#state`（Claude 页面展示的格式）。

local CLIENT_ID = "9d1c250a-e61b-44d9-88ed-5944d1962f5e"
local AUTH_URL = "https://claude.ai/oauth/authorize"
local TOKEN_URL = "https://platform.claude.com/v1/oauth/token"
local PROFILE_URL = "https://api.anthropic.com/api/oauth/profile"
local REDIRECT_URI = "http://localhost:54545/callback"
local CALLBACK_PORT = 54545
local SCOPE = "user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload"

MB = {
  name = "auth-claude",
  version = "0.1.0",
  category = "auth",
  scopes = { "provider" },
  capabilities = { "auth" },
}

local function urlencode(s)
  return (s:gsub("([^%w%-%._~])", function(c)
    return string.format("%%%02X", string.byte(c))
  end))
end

-- hex 摘要 → 原始字节串（sha256 返回 hex，PKCE challenge 需要 raw digest）
local function hex_to_bytes(h)
  return (h:gsub("..", function(cc) return string.char(tonumber(cc, 16)) end))
end

local function pkce_pair()
  local verifier = mb.crypto.base64url_encode(mb.random.bytes(32))
  return verifier, mb.crypto.base64url_encode(hex_to_bytes(mb.crypto.sha256(verifier)))
end

-- token 响应 → 令牌包。expires_at 记服务端原始过期时刻（unix 毫秒）。
local function bundle_from_token(body, refresh_fallback)
  if type(body) ~= "table" then error("Claude token 响应不是合法 JSON") end
  local access = body.access_token
  if type(access) ~= "string" or access == "" then error("Claude token 响应缺少 access_token") end
  local expires_in = tonumber(body.expires_in)
  if not expires_in or expires_in < 0 then error("Claude token 响应缺少合法 expires_in") end
  local refresh = body.refresh_token
  if type(refresh) ~= "string" or refresh == "" then refresh = refresh_fallback end
  if not refresh then error("Claude token 响应缺少 refresh_token") end
  return {
    access = access,
    refresh = refresh,
    expires_at = mb.time.now_ms() + math.floor(expires_in * 1000),
    source = "oauth",
  }
end

-- 账户身份（advisory）：profile 端点给 uuid/email；失败不阻断登录
local function fetch_profile(access)
  local ok, r = pcall(mb.http.request, {
    method = "GET",
    url = PROFILE_URL,
    headers = { { "authorization", "Bearer " .. access }, { "accept", "application/json" } },
    timeout_ms = 10000,
  })
  if not ok or r.status ~= 200 or type(r.body) ~= "table" then return nil, nil end
  local acc = r.body.account
  if type(acc) ~= "table" then return nil, nil end
  return acc.uuid, acc.email
end

-- 授权码 → 令牌包（JSON 交换体；state 随包回传由 Anthropic 复核）
local function exchange(code, state, verifier)
  local r = mb.http.request({
    method = "POST",
    url = TOKEN_URL,
    headers = { { "accept", "application/json" } },
    body = {
      grant_type = "authorization_code",
      code = code,
      redirect_uri = REDIRECT_URI,
      client_id = CLIENT_ID,
      code_verifier = verifier,
      state = state,
    },
  })
  if r.status ~= 200 then
    local desc = (type(r.body) == "table") and (r.body.error_description or r.body.error) or ""
    error("Claude 授权码交换失败（HTTP " .. r.status .. "）" .. tostring(desc))
  end
  local b = bundle_from_token(r.body, nil)
  local account, email = fetch_profile(b.access)
  b.account = account
  b.email = email
  return b
end

-- 粘贴解析：callback URL / `code#state`（Claude 页面展示格式）/ 裸 code+state 不判
local function parse_paste(text, expect_state)
  if type(text) ~= "string" or text == "" then return nil, "空输入" end
  local s = text:match("^%s*(.-)%s*$")
  -- code#state（官方粘贴格式）
  local code, st = s:match("^([%w%-%._~]+)#([%w%-%._~]+)$")
  if code then
    if st ~= expect_state then return nil, "state 不匹配（可能粘贴了旧会话的授权码）" end
    return { code = code }
  end
  -- 完整回调 URL：...?code=..&state=..
  if s:match("^https?://") then
    local q = s:match("%?(.*)$")
    if not q then return nil, "URL 中无查询参数" end
    local params = {}
    for kv in q:gmatch("[^&]+") do
      local k, v = kv:match("^([%w%-%._~]+)=(.*)$")
      if k then params[k] = v end
    end
    if params.error then return nil, "授权被拒: " .. tostring(params.error) end
    if not params.code then return nil, "URL 中缺少 code 参数" end
    if params.state and params.state ~= expect_state then
      return nil, "state 不匹配（可能粘贴了旧会话的回调链接）"
    end
    return { code = params.code }
  end
  return nil, "无法识别输入——请粘贴回调 URL 或 code#state"
end

function MB.auth_describe(ctx)
  return {
    kind = "callback",
    label = "Claude 账户",
    supports_paste = true,
    instructions = "浏览器完成授权后自动回跳；也可粘贴回调 URL 或 code#state",
    provider = {
      key = "claude-oauth",
      label = "Claude",
      protocol = "anthropic",
      base_url = "https://api.anthropic.com",
      dashboard_url = "https://claude.ai/settings/usage",
      models_dev_id = "anthropic",
      note = "Claude Pro/Max 订阅账户（浏览器授权登录）",
    },
  }
end

function MB.auth_begin(ctx)
  local state = mb.random.state()
  local verifier, challenge = pkce_pair()
  local l = mb.oauth.listen_callback({
    path = "/callback",
    preferred_port = CALLBACK_PORT,
  })
  if l.port ~= CALLBACK_PORT then
    mb.oauth.callback_close(l.id)
    error("回调端口 " .. CALLBACK_PORT .. " 被占用（Anthropic 注册的 redirect_uri 固定），请释放后重试")
  end
  local url = AUTH_URL
    .. "?code=true&client_id=" .. urlencode(CLIENT_ID)
    .. "&response_type=code&redirect_uri=" .. urlencode(REDIRECT_URI)
    .. "&scope=" .. urlencode(SCOPE)
    .. "&code_challenge=" .. challenge .. "&code_challenge_method=S256"
    .. "&state=" .. state
  return {
    verification_url = url,
    expires_in_secs = 300,
    handle = { listener = l.id, state = state, verifier = verifier },
  }
end

function MB.auth_poll(ctx, handle, paste)
  -- 手动粘贴优先（回调 URL / code#state）
  if type(paste) == "string" and paste ~= "" then
    local parsed, err = parse_paste(paste, handle.state)
    if not parsed then
      return { status = "pending", message = err }
    end
    mb.oauth.callback_close(handle.listener)
    local ok, b = pcall(exchange, parsed.code, handle.state, handle.verifier)
    if not ok then
      return { status = "pending", message = tostring(b) }
    end
    return { status = "done", bundle = b }
  end
  -- 浏览器回调（1s 心跳，编排器掌握总超时与取消）
  local r = mb.oauth.callback_await(handle.listener, 1000)
  if not r then return { status = "pending" } end
  mb.oauth.callback_close(handle.listener)
  if r.error then
    return { status = "error", message = "授权被拒: " .. tostring(r.error) }
  end
  if r.state ~= handle.state then
    return { status = "error", message = "state 不匹配（可能是旧会话的回调）" }
  end
  if not r.code or r.code == "" then
    return { status = "error", message = "回调缺少 code 参数" }
  end
  local ok, b = pcall(exchange, r.code, r.state, handle.verifier)
  if not ok then
    return { status = "error", message = tostring(b) }
  end
  return { status = "done", bundle = b }
end

function MB.auth_refresh(ctx, bundle)
  local r = mb.http.request({
    method = "POST",
    url = TOKEN_URL,
    headers = { { "accept", "application/json" } },
    body = {
      grant_type = "refresh_token",
      refresh_token = bundle.refresh,
      client_id = CLIENT_ID,
    },
  })
  if r.status ~= 200 then
    local desc = (type(r.body) == "table") and (r.body.error_description or r.body.error) or ""
    error("Claude 令牌刷新被拒绝（HTTP " .. r.status .. "），请重新登录 " .. tostring(desc))
  end
  local b = bundle_from_token(r.body, bundle.refresh)
  b.account = bundle.account
  b.email = bundle.email
  return b
end

function MB.auth_headers(ctx, bundle)
  return {
    { "authorization", "Bearer " .. bundle.access },
    { "anthropic-beta", "oauth-2025-04-20" },
  }
end
