--- Codex（ChatGPT 订阅）账户认证插件（PKCE 授权码流，对齐 codex CLI）。
---
--- 令牌包契约（见 plugins/moonbridge.lua 的 MB.auth_* 段）：core 只认
--- access / expires_at（服务端原始过期时刻，unix 毫秒；skew 由 core 统一扣），
--- 其余字段（refresh / id_token / account_id / account / source）是本插件私有、随包透传。
---
--- 两个凭据来源：浏览器授权（redirect_uri 是注册的固定值
--- http://localhost:1455/auth/callback，端口占用则显式报错）；
--- 本地 Codex CLI 凭据导入（~/.codex/auth.json，fs_read_allow 白名单）。
---
--- 出站：ChatGPT 订阅走 chatgpt.com/backend-api/codex（OpenAI Responses 形态），
--- 认证头 = Bearer access_token + chatgpt-account-id（id_token 的 JWT 声明）。

local CLIENT_ID = "app_EMoamEEZ73f0CkXaXp7hrann"
local AUTH_URL = "https://auth.openai.com/oauth/authorize"
local TOKEN_URL = "https://auth.openai.com/oauth/token"
local REDIRECT_URI = "http://localhost:1455/auth/callback"
local CALLBACK_PORT = 1455
local SCOPE = "openid email profile offline_access"
local CLI_AUTH_FILE = "~/.codex/auth.json"

MB = {
  name = "auth-codex",
  version = "0.1.0",
  category = "auth",
  scopes = { "provider" },
  capabilities = { "auth" },
  -- mb.fs.read 白名单（仅此一份文件，精确匹配）
  fs_read_allow = { CLI_AUTH_FILE },
}

local function urlencode(s)
  return (s:gsub("([^%w%-%._~])", function(c)
    return string.format("%%%02X", string.byte(c))
  end))
end

local function hex_to_bytes(h)
  return (h:gsub("..", function(cc) return string.char(tonumber(cc, 16)) end))
end

local function pkce_pair()
  local verifier = mb.crypto.base64url_encode(mb.random.bytes(32))
  return verifier, mb.crypto.base64url_encode(hex_to_bytes(mb.crypto.sha256(verifier)))
end

-- 解码 JWT payload（不验签——只取身份声明，令牌有效性由服务端裁决）
local function jwt_payload(token)
  if type(token) ~= "string" then return nil end
  local payload = token:match("^[^.]+%.([^.]+)%.[^.]+$")
  if not payload then return nil end
  local ok, s = pcall(mb.crypto.base64url_decode, payload)
  if not ok then return nil end
  return s
end

local function json_str(text, key)
  if not text then return nil end
  local v = text:match('"' .. key .. '"%s*:%s*"([^"]+)"')
  if v == "" then return nil end
  return v
end

-- token 响应 → 令牌包。expires_at 记服务端原始过期时刻（unix 毫秒）。
local function bundle_from_token(body, refresh_fallback)
  if type(body) ~= "table" then error("Codex token 响应不是合法 JSON") end
  local access = body.access_token
  if type(access) ~= "string" or access == "" then error("Codex token 响应缺少 access_token") end
  local expires_in = tonumber(body.expires_in)
  if not expires_in or expires_in < 0 then error("Codex token 响应缺少合法 expires_in") end
  local refresh = body.refresh_token
  if type(refresh) ~= "string" or refresh == "" then refresh = refresh_fallback end
  if not refresh then error("Codex token 响应缺少 refresh_token") end
  local idt = body.id_token
  local claims = jwt_payload(idt) or jwt_payload(access)
  local b = {
    access = access,
    refresh = refresh,
    expires_at = mb.time.now_ms() + math.floor(expires_in * 1000),
    account_id = json_str(claims, "chatgpt_account_id"),
    account = json_str(claims, "email") or json_str(claims, "sub"),
    source = "oauth",
  }
  if type(idt) == "string" then b.id_token = idt end
  return b
end

-- 授权码 → 令牌包（form 编码交换体）
local function exchange(code, verifier)
  local r = mb.http.request({
    method = "POST",
    url = TOKEN_URL,
    headers = { { "accept", "application/json" } },
    form = {
      { "grant_type", "authorization_code" },
      { "client_id", CLIENT_ID },
      { "code", code },
      { "redirect_uri", REDIRECT_URI },
      { "code_verifier", verifier },
    },
  })
  if r.status ~= 200 then
    local desc = (type(r.body) == "table") and (r.body.error_description or r.body.error) or ""
    error("Codex 授权码交换失败（HTTP " .. r.status .. "）" .. tostring(desc))
  end
  return bundle_from_token(r.body, nil)
end

-- ~/.codex/auth.json → 令牌包（codex CLI 凭据格式：
-- { tokens = { access_token, id_token, refresh_token, account_id }, last_refresh }。
-- 极简 JSON 提取：字段名在文件内唯一，正则取值即可）
local function import_local()
  local ok, text = pcall(mb.fs.read, CLI_AUTH_FILE)
  if not ok or type(text) ~= "string" or text == "" then
    return nil, "未找到 Codex CLI 凭据（" .. CLI_AUTH_FILE .. "）"
  end
  local access = text:match('"access_token"%s*:%s*"([^"]+)"')
  if not access or access == "" then
    return nil, "Codex CLI 凭据缺少 access_token"
  end
  local refresh = text:match('"refresh_token"%s*:%s*"([^"]+)"')
  local id_token = text:match('"id_token"%s*:%s*"([^"]+)"')
  -- 过期时刻取 JWT exp（服务端声明值；缺 expires_in 时 CLI 也这么判断）
  local exp = tonumber(json_str(jwt_payload(access), "exp"))
    or tonumber(json_str(jwt_payload(id_token), "exp"))
  local b = {
    access = access,
    refresh = refresh,
    expires_at = exp and math.floor(exp * 1000) or nil,
    account_id = text:match('"account_id"%s*:%s*"([^"]+)"'),
    source = "local-cli",
  }
  if id_token then b.id_token = id_token end
  if not b.account_id then
    b.account_id = json_str(jwt_payload(id_token), "chatgpt_account_id")
  end
  local claims = jwt_payload(id_token) or jwt_payload(access)
  b.account = json_str(claims, "email") or json_str(claims, "sub")
  return b
end

local function parse_paste(text, expect_state)
  if type(text) ~= "string" or text == "" then return nil, "空输入" end
  local s = text:match("^%s*(.-)%s*$")
  if not s:match("^https?://") then return nil, "请粘贴完整回调 URL（http://localhost:1455/auth/callback?...）" end
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

function MB.auth_describe(ctx)
  return {
    kind = "callback",
    label = "Codex 账户",
    supports_paste = true,
    instructions = "浏览器完成授权后自动回跳；也可粘贴回调 URL",
    sources = {
      { id = "local-cli", label = "导入 Codex CLI 凭据" },
      { id = "browser", label = "浏览器授权登录" },
    },
    provider = {
      key = "codex-oauth",
      label = "Codex",
      protocol = "openai-response",
      base_url = "https://chatgpt.com/backend-api/codex",
      models_dev_id = "openai",
      note = "ChatGPT 订阅（Codex）账户登录 / Codex CLI 凭据导入",
    },
  }
end

function MB.auth_begin(ctx)
  -- 本地导入优先命中则直出（~/.codex/auth.json）
  if ctx.source == "local-cli" then
    local b, reason = import_local()
    if b then return { status = "done", bundle = b } end
    return { status = "error", message = reason }
  end
  local state = mb.random.state()
  local verifier, challenge = pkce_pair()
  local l = mb.oauth.listen_callback({
    path = "/auth/callback",
    preferred_port = CALLBACK_PORT,
  })
  if l.port ~= CALLBACK_PORT then
    mb.oauth.callback_close(l.id)
    error("回调端口 " .. CALLBACK_PORT .. " 被占用（OpenAI 注册的 redirect_uri 固定），请释放后重试")
  end
  local url = AUTH_URL
    .. "?client_id=" .. urlencode(CLIENT_ID)
    .. "&response_type=code&redirect_uri=" .. urlencode(REDIRECT_URI)
    .. "&scope=" .. urlencode(SCOPE) .. "&state=" .. state
    .. "&code_challenge=" .. challenge .. "&code_challenge_method=S256"
    .. "&prompt=login&id_token_add_organizations=true&codex_cli_simplified_flow=true"
  return {
    verification_url = url,
    expires_in_secs = 300,
    handle = { listener = l.id, state = state, verifier = verifier },
  }
end

function MB.auth_poll(ctx, handle, paste)
  if type(paste) == "string" and paste ~= "" then
    local parsed, err = parse_paste(paste, handle.state)
    if not parsed then
      return { status = "pending", message = err }
    end
    mb.oauth.callback_close(handle.listener)
    local ok, b = pcall(exchange, parsed.code, handle.verifier)
    if not ok then
      return { status = "pending", message = tostring(b) }
    end
    return { status = "done", bundle = b }
  end
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
  local ok, b = pcall(exchange, r.code, handle.verifier)
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
    form = {
      { "grant_type", "refresh_token" },
      { "client_id", CLIENT_ID },
      { "refresh_token", bundle.refresh },
    },
  })
  if r.status ~= 200 then
    local desc = (type(r.body) == "table") and (r.body.error_description or r.body.error) or ""
    error("Codex 令牌刷新被拒绝（HTTP " .. r.status .. "），请重新登录 " .. tostring(desc))
  end
  local b = bundle_from_token(r.body, bundle.refresh)
  -- 响应不带 id_token 时保留旧的（account_id 声明不变）
  b.account_id = b.account_id or bundle.account_id
  return b
end

function MB.auth_headers(ctx, bundle)
  local h = {
    { "authorization", "Bearer " .. bundle.access },
    { "originator", "codex_cli_rs" },
    { "OpenAI-Beta", "responses=experimental" },
  }
  if bundle.account_id and bundle.account_id ~= "" then
    h[#h + 1] = { "chatgpt-account-id", bundle.account_id }
  end
  return h
end
