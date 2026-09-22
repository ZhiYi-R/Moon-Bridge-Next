--- Qwen 账户认证插件（RFC 8628 设备码 + PKCE，对齐 qwen-code CLI）。
---
--- 令牌包契约（见 plugins/moonbridge.lua 的 MB.auth_* 段）：core 只认
--- access / expires_at（服务端原始过期时刻，unix 毫秒；skew 由 core 统一扣），
--- 其余字段（refresh / resource_url / account / source）是本插件私有、随包透传。
---
--- 两个凭据来源：设备码授权（浏览器验证码）；
--- 本地 Qwen Code CLI 凭据导入（~/.qwen/oauth_creds.json，fs_read_allow 白名单）。
--- resource_url：token 响应/凭据文件可给出专属资源端点，出站 header 不变。

local OAUTH_BASE = "https://chat.qwen.ai"
local CLIENT_ID = "f0304373b74a44d2b584a3fb70ca9e56"
local SCOPE = "openid profile email model.completion"
local CLI_CREDS_FILE = "~/.qwen/oauth_creds.json"
local DEFAULT_BASE_URL = "https://portal.qwen.ai/v1"

MB = {
  name = "auth-qwen",
  version = "0.1.0",
  category = "auth",
  scopes = { "provider" },
  capabilities = { "auth" },
  -- mb.fs.read 白名单（仅此一份文件，精确匹配）
  fs_read_allow = { CLI_CREDS_FILE },
}

local function hex_to_bytes(h)
  return (h:gsub("..", function(cc) return string.char(tonumber(cc, 16)) end))
end

local function pkce_pair()
  local verifier = mb.crypto.base64url_encode(mb.random.bytes(32))
  return verifier, mb.crypto.base64url_encode(hex_to_bytes(mb.crypto.sha256(verifier)))
end

-- token 响应 → 令牌包。expires_at 记服务端原始过期时刻（unix 毫秒）；
-- resource_url 是 qwen 的专属资源端点（token 响应可给，刷新时透传）。
local function bundle_from_token(body, refresh_fallback)
  if type(body) ~= "table" then error("Qwen token 响应不是合法 JSON") end
  local access = body.access_token
  if type(access) ~= "string" or access == "" then error("Qwen token 响应缺少 access_token") end
  local expires_in = tonumber(body.expires_in)
  if not expires_in or expires_in < 0 then error("Qwen token 响应缺少合法 expires_in") end
  local refresh = body.refresh_token
  if type(refresh) ~= "string" or refresh == "" then refresh = refresh_fallback end
  if not refresh then error("Qwen token 响应缺少 refresh_token") end
  local b = {
    access = access,
    refresh = refresh,
    expires_at = mb.time.now_ms() + math.floor(expires_in * 1000),
    source = "oauth",
  }
  if type(body.resource_url) == "string" and body.resource_url ~= "" then
    b.resource_url = body.resource_url
  end
  return b
end

-- ~/.qwen/oauth_creds.json → 令牌包（qwen-code 凭据格式：
-- { access_token, refresh_token, expiry_date, resource_url }，字段名唯一故正则取值）
local function import_local()
  local ok, text = pcall(mb.fs.read, CLI_CREDS_FILE)
  if not ok or type(text) ~= "string" or text == "" then
    return nil, "未找到 Qwen CLI 凭据（" .. CLI_CREDS_FILE .. "）"
  end
  local access = text:match('"access_token"%s*:%s*"([^"]+)"')
  if not access or access == "" then
    return nil, "Qwen CLI 凭据缺少 access_token"
  end
  local expiry = tonumber(text:match('"expiry_date"%s*:%s*(%d+)'))
  local b = {
    access = access,
    refresh = text:match('"refresh_token"%s*:%s*"([^"]+)"'),
    expires_at = expiry,
    resource_url = text:match('"resource_url"%s*:%s*"([^"]+)"'),
    source = "local-cli",
  }
  if b.resource_url == "" then b.resource_url = nil end
  return b
end

function MB.auth_describe(ctx)
  return {
    kind = "device_code",
    label = "Qwen 账户",
    instructions = "打开验证页，输入验证码完成授权",
    sources = {
      { id = "local-cli", label = "导入 Qwen CLI 凭据" },
      { id = "device", label = "设备码授权登录" },
    },
    provider = {
      key = "qwen-oauth",
      label = "Qwen",
      protocol = "openai-chat",
      base_url = DEFAULT_BASE_URL,
      models_dev_id = "qwen",
      note = "Qwen 账户设备码登录 / Qwen Code CLI 凭据导入",
    },
  }
end

function MB.auth_begin(ctx)
  if ctx.source == "local-cli" then
    local b, reason = import_local()
    if b then return { status = "done", bundle = b } end
    return { status = "error", message = reason }
  end
  local verifier, challenge = pkce_pair()
  local r = mb.http.request({
    method = "POST",
    url = OAUTH_BASE .. "/api/v1/oauth2/device/code",
    headers = { { "accept", "application/json" } },
    form = {
      { "client_id", CLIENT_ID },
      { "scope", SCOPE },
      { "code_challenge", challenge },
      { "code_challenge_method", "S256" },
    },
  })
  if r.status ~= 200 then error("Qwen 设备授权请求失败（HTTP " .. r.status .. "）") end
  local b = r.body
  if type(b) ~= "table" or not b.user_code or not b.device_code then
    error("Qwen 设备授权响应缺少 user_code/device_code")
  end
  local verification = b.verification_uri_complete or b.verification_uri
  if not verification then error("Qwen 设备授权响应缺少 verification_uri") end
  return {
    verification_url = verification,
    user_code = b.user_code,
    interval_secs = tonumber(b.interval) or 5,
    expires_in_secs = tonumber(b.expires_in) or 900,
    handle = { device_code = b.device_code, verifier = verifier },
  }
end

function MB.auth_poll(ctx, handle)
  local r = mb.http.request({
    method = "POST",
    url = OAUTH_BASE .. "/api/v1/oauth2/token",
    headers = { { "accept", "application/json" } },
    form = {
      { "client_id", CLIENT_ID },
      { "device_code", handle.device_code },
      { "code_verifier", handle.verifier },
      { "grant_type", "urn:ietf:params:oauth:grant-type:device_code" },
    },
  })
  if r.status == 200 then
    return { status = "done", bundle = bundle_from_token(r.body, nil) }
  end
  local err = (type(r.body) == "table") and r.body.error or nil
  if err == "authorization_pending" then return { status = "pending" } end
  if err == "slow_down" then
    local iv = tonumber((type(r.body) == "table") and r.body.interval or nil)
    return { status = "slow_down", interval_secs = (iv and iv > 0) and iv or 10 }
  end
  if err == "expired_token" then return { status = "expired", message = "Qwen 设备授权已过期，请重新发起登录" } end
  if err == "access_denied" then return { status = "error", message = "Qwen 设备授权被拒绝" } end
  local desc = (type(r.body) == "table") and r.body.error_description or ""
  return { status = "error", message = "Qwen 授权失败（HTTP " .. r.status .. "）" .. tostring(desc) }
end

function MB.auth_refresh(ctx, bundle)
  local r = mb.http.request({
    method = "POST",
    url = OAUTH_BASE .. "/api/v1/oauth2/token",
    headers = { { "accept", "application/json" } },
    form = {
      { "grant_type", "refresh_token" },
      { "client_id", CLIENT_ID },
      { "refresh_token", bundle.refresh },
    },
  })
  if r.status ~= 200 then
    local desc = (type(r.body) == "table") and (r.body.error_description or r.body.error) or ""
    error("Qwen 令牌刷新被拒绝（HTTP " .. r.status .. "），请重新登录 " .. tostring(desc))
  end
  local b = bundle_from_token(r.body, bundle.refresh)
  b.resource_url = b.resource_url or bundle.resource_url
  return b
end

function MB.auth_headers(ctx, bundle)
  return { { "authorization", "Bearer " .. bundle.access } }
end
