--- Grok（xAI）账户认证插件（RFC 8628 设备码流；端点走 OIDC discovery）。
---
--- 令牌包契约（见 plugins/moonbridge.lua 的 MB.auth_* 段）：core 只认
--- access / expires_at（服务端原始过期时刻，unix 毫秒；skew 由 core 统一扣），
--- 其余字段（refresh / token_endpoint / account / email / source）是本插件私有、
--- 随包透传。
---
--- discovery：https://auth.x.ai/.well-known/openid-configuration 给出
--- device_authorization_endpoint 与 token_endpoint；token_endpoint 记入令牌包，
--- 刷新时直接用（不重复 discovery）。

local ISSUER = "https://auth.x.ai"
local DISCOVERY_URL = ISSUER .. "/.well-known/openid-configuration"
local CLIENT_ID = "b1a00492-073a-47ea-816f-4c329264a828"
local SCOPE = "openid profile email offline_access grok-cli:access api:access"

MB = {
  name = "auth-grok",
  version = "0.1.0",
  category = "auth",
  scopes = { "provider" },
  capabilities = { "auth" },
}

-- OIDC discovery → { device_authorization_endpoint, token_endpoint }
local function discover()
  local r = mb.http.request({
    method = "GET",
    url = DISCOVERY_URL,
    headers = { { "accept", "application/json" } },
  })
  if r.status ~= 200 or type(r.body) ~= "table" then
    error("xAI OIDC discovery 失败（HTTP " .. r.status .. "）")
  end
  local dev = r.body.device_authorization_endpoint
  local tok = r.body.token_endpoint
  if type(dev) ~= "string" or type(tok) ~= "string" or dev == "" or tok == "" then
    error("xAI OIDC discovery 响应缺少 device_authorization_endpoint/token_endpoint")
  end
  -- 端点校验：只允许 auth.x.ai 下的路径（discovery 文档被篡改时不得外送凭据）
  if not dev:match("^https://auth%.x%.ai/") or not tok:match("^https://auth%.x%.ai/") then
    error("xAI OIDC discovery 返回了非 auth.x.ai 的端点")
  end
  return dev, tok
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
local function bundle_from_token(body, refresh_fallback, token_endpoint)
  if type(body) ~= "table" then error("xAI token 响应不是合法 JSON") end
  local access = body.access_token
  if type(access) ~= "string" or access == "" then error("xAI token 响应缺少 access_token") end
  local expires_in = tonumber(body.expires_in)
  if not expires_in or expires_in < 0 then error("xAI token 响应缺少合法 expires_in") end
  local refresh = body.refresh_token
  if type(refresh) ~= "string" or refresh == "" then refresh = refresh_fallback end
  if not refresh then error("xAI token 响应缺少 refresh_token") end
  local claims = jwt_payload(access) or jwt_payload(body.id_token)
  return {
    access = access,
    refresh = refresh,
    expires_at = mb.time.now_ms() + math.floor(expires_in * 1000),
    token_endpoint = token_endpoint,
    account = json_str(claims, "sub") or json_str(claims, "user_id"),
    email = json_str(claims, "email"),
    source = "oauth",
  }
end

function MB.auth_describe(ctx)
  return {
    kind = "device_code",
    label = "Grok 账户",
    instructions = "打开验证页，输入验证码完成授权",
    provider = {
      key = "grok-oauth",
      label = "Grok",
      protocol = "openai-chat",
      base_url = "https://api.x.ai/v1",
      dashboard_url = "https://console.x.ai",
      models_dev_id = "xai",
      note = "xAI Grok 账户设备码登录（浏览器验证码授权）",
    },
  }
end

function MB.auth_begin(ctx)
  local device_endpoint, token_endpoint = discover()
  local r = mb.http.request({
    method = "POST",
    url = device_endpoint,
    headers = { { "accept", "application/json" } },
    form = {
      { "client_id", CLIENT_ID },
      { "scope", SCOPE },
    },
  })
  if r.status ~= 200 then error("xAI 设备授权请求失败（HTTP " .. r.status .. "）") end
  local b = r.body
  if type(b) ~= "table" or not b.user_code or not b.device_code then
    error("xAI 设备授权响应缺少 user_code/device_code")
  end
  local verification = b.verification_uri_complete or b.verification_uri
  if not verification then error("xAI 设备授权响应缺少 verification_uri") end
  return {
    verification_url = verification,
    user_code = b.user_code,
    interval_secs = tonumber(b.interval) or 5,
    expires_in_secs = tonumber(b.expires_in) or 900,
    handle = { device_code = b.device_code, token_endpoint = token_endpoint },
  }
end

function MB.auth_poll(ctx, handle)
  local r = mb.http.request({
    method = "POST",
    url = handle.token_endpoint,
    headers = { { "accept", "application/json" } },
    form = {
      { "client_id", CLIENT_ID },
      { "device_code", handle.device_code },
      { "grant_type", "urn:ietf:params:oauth:grant-type:device_code" },
    },
  })
  if r.status == 200 then
    return { status = "done", bundle = bundle_from_token(r.body, nil, handle.token_endpoint) }
  end
  local err = (type(r.body) == "table") and r.body.error or nil
  if err == "authorization_pending" then return { status = "pending" } end
  if err == "slow_down" then
    local iv = tonumber((type(r.body) == "table") and r.body.interval or nil)
    return { status = "slow_down", interval_secs = (iv and iv > 0) and iv or 10 }
  end
  if err == "expired_token" then return { status = "expired", message = "xAI 设备授权已过期，请重新发起登录" } end
  if err == "access_denied" then return { status = "error", message = "xAI 设备授权被拒绝" } end
  local desc = (type(r.body) == "table") and r.body.error_description or ""
  return { status = "error", message = "xAI 授权失败（HTTP " .. r.status .. "）" .. tostring(desc) }
end

function MB.auth_refresh(ctx, bundle)
  -- token_endpoint 随包透传（无则重新 discovery 兜底）
  local token_endpoint = bundle.token_endpoint
  if not token_endpoint then
    local _, tok = discover()
    token_endpoint = tok
  end
  local r = mb.http.request({
    method = "POST",
    url = token_endpoint,
    headers = { { "accept", "application/json" } },
    form = {
      { "grant_type", "refresh_token" },
      { "client_id", CLIENT_ID },
      { "refresh_token", bundle.refresh },
    },
  })
  if r.status ~= 200 then
    local desc = (type(r.body) == "table") and (r.body.error_description or r.body.error) or ""
    error("xAI 令牌刷新被拒绝（HTTP " .. r.status .. "），请重新登录 " .. tostring(desc))
  end
  return bundle_from_token(r.body, bundle.refresh, token_endpoint)
end

function MB.auth_headers(ctx, bundle)
  return { { "authorization", "Bearer " .. bundle.access } }
end
