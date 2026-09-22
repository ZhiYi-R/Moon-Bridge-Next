--- Kimi 账户认证插件（RFC 8628 设备码流）。
---
--- 令牌包契约（见 plugins/moonbridge.lua 的 MB.auth_* 段）：core 只认
--- access / expires_at（服务端原始过期时刻，unix 毫秒；skew 由 core 统一扣），
--- 其余字段（refresh / device_id / account / email / source）是本插件私有、随包透传。
---
--- mb.config 注入项：
---   host_override  OAuth 主机（默认 https://auth.kimi.com；镜像/测试用）

local HOST = (mb.config and mb.config.host_override) or "https://auth.kimi.com"
local CLIENT_ID = "17e5f671-d194-4dfb-9706-5516cb48c098"
local CLI_VERSION = "0.14.0"

MB = {
  name = "auth-kimi",
  version = "0.1.0",
  scopes = { "provider" },
  capabilities = { "auth" },
}

-- 设备 id：安装级稳定，存于加密小值（命名空间隔离在本插件下）
local function device_id()
  local id = mb.secret.get("meta", "device_id")
  if id and id ~= "" then return id end
  id = mb.random.state()
  mb.secret.set("meta", "device_id", id)
  return id
end

-- KimiCLI 身份头（服务端按此识别客户端形态；主机信息由宿主经 ctx.host 提供）
local function cli_headers(device, ctx)
  local h = {
    { "user-agent", "KimiCLI/" .. CLI_VERSION },
    { "X-Msh-Platform", "kimi_code_cli" },
    { "X-Msh-Version", CLI_VERSION },
    { "X-Msh-Device-Id", device },
  }
  local host = ctx and ctx.host
  if host then
    h[#h + 1] = { "X-Msh-Device-Name", (host.name and host.name ~= "") and host.name or "unknown" }
    h[#h + 1] = { "X-Msh-Device-Model", tostring(host.os) .. " " .. tostring(host.arch) }
    h[#h + 1] = { "X-Msh-Os-Version", tostring(host.os) }
  end
  return h
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

-- 极简 JSON 字符串取值（身份展示用；完整解析不必要，凭据不依赖它）
local function json_str(text, key)
  if not text then return nil end
  local v = text:match('"' .. key .. '"%s*:%s*"([^"]+)"')
  if v == "" then return nil end
  return v
end

local function identity(access, refresh)
  local a, r = jwt_payload(access), jwt_payload(refresh)
  local account = json_str(a, "user_id") or json_str(a, "sub")
    or json_str(r, "user_id") or json_str(r, "sub")
  local email = json_str(a, "email") or json_str(r, "email")
  if email then email = email:lower() end
  return account, email
end

-- token 响应 → 令牌包。expires_at 记服务端原始过期时刻（unix 毫秒）。
local function bundle_from_token(body, refresh_fallback, device)
  if type(body) ~= "table" then error("Kimi token 响应不是合法 JSON") end
  local access = body.access_token
  if type(access) ~= "string" or access == "" then error("Kimi token 响应缺少 access_token") end
  local expires_in = tonumber(body.expires_in)
  if not expires_in or expires_in < 0 then error("Kimi token 响应缺少合法 expires_in") end
  local refresh = body.refresh_token
  if type(refresh) ~= "string" or refresh == "" then refresh = refresh_fallback end
  if not refresh then error("Kimi token 响应缺少 refresh_token") end
  local account, email = identity(access, refresh)
  return {
    access = access,
    refresh = refresh,
    expires_at = mb.time.now_ms() + math.floor(expires_in * 1000),
    device_id = device,
    account = account,
    email = email,
    source = "oauth",
  }
end

function MB.auth_describe(ctx)
  return {
    kind = "device_code",
    label = "Kimi 账户",
    instructions = "打开验证页，输入验证码完成授权",
  }
end

function MB.auth_begin(ctx)
  local device = device_id()
  local r = mb.http.request({
    method = "POST",
    url = HOST .. "/api/oauth/device_authorization",
    headers = cli_headers(device, ctx),
    form = { { "client_id", CLIENT_ID } },
  })
  if r.status ~= 200 then error("Kimi 设备授权请求失败（HTTP " .. r.status .. "）") end
  local b = r.body
  if type(b) ~= "table" or not b.user_code or not b.device_code then
    error("Kimi 设备授权响应缺少 user_code/device_code")
  end
  local verification = b.verification_uri_complete or b.verification_uri
  if not verification then error("Kimi 设备授权响应缺少 verification_uri") end
  return {
    verification_url = verification,
    user_code = b.user_code,
    interval_secs = tonumber(b.interval) or 5,
    expires_in_secs = tonumber(b.expires_in) or 900,
    handle = { device_code = b.device_code },
  }
end

function MB.auth_poll(ctx, handle)
  local device = device_id()
  local r = mb.http.request({
    method = "POST",
    url = HOST .. "/api/oauth/token",
    headers = cli_headers(device, ctx),
    form = {
      { "client_id", CLIENT_ID },
      { "device_code", handle.device_code },
      { "grant_type", "urn:ietf:params:oauth:grant-type:device_code" },
    },
  })
  if r.status == 200 then
    return { status = "done", bundle = bundle_from_token(r.body, nil, device) }
  end
  local err = (type(r.body) == "table") and r.body.error or nil
  if err == "authorization_pending" then return { status = "pending" } end
  if err == "slow_down" then
    local iv = tonumber((type(r.body) == "table") and r.body.interval or nil)
    return { status = "slow_down", interval_secs = (iv and iv > 0) and iv or 10 }
  end
  if err == "expired_token" then return { status = "expired", message = "Kimi 设备授权已过期，请重新发起登录" } end
  if err == "access_denied" then return { status = "error", message = "Kimi 设备授权被拒绝" } end
  local desc = (type(r.body) == "table") and r.body.error_description or ""
  return { status = "error", message = "Kimi 授权失败（HTTP " .. r.status .. "）" .. tostring(desc) }
end

function MB.auth_refresh(ctx, bundle)
  local device = bundle.device_id or device_id()
  local r = mb.http.request({
    method = "POST",
    url = HOST .. "/api/oauth/token",
    headers = cli_headers(device, ctx),
    form = {
      { "grant_type", "refresh_token" },
      { "refresh_token", bundle.refresh },
      { "client_id", CLIENT_ID },
    },
  })
  if r.status ~= 200 then
    local desc = (type(r.body) == "table") and r.body.error_description or ""
    error("Kimi 令牌刷新被拒绝（HTTP " .. r.status .. "），请重新登录 " .. tostring(desc))
  end
  return bundle_from_token(r.body, bundle.refresh, device)
end

function MB.auth_headers(ctx, bundle)
  return { { "authorization", "Bearer " .. bundle.access } }
end
