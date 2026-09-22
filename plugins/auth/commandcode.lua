--- Command Code 账户认证插件（本地 CLI 导入 / 浏览器回调 / 手动粘贴）。
---
--- 登录产物是长期 API key（无刷新端点）：令牌包不带 expires_at，core 永不
--- 触发刷新；401 即需重新登录（auth_refresh 只会报错并说明）。
---
--- 来源选择（落实「显式选来源，whoami 失败必须暴露」）：编排器经 ctx.source
--- 传入用户选择——"local-cli" 只走本地导入（失败即显式报错）；"browser" 只走
--- 浏览器回调；缺省先尝试本地导入、失败自动回落浏览器（notice 告知）。
---
--- mb.config 注入项：
---   studio_override   Studio 站点（默认 https://commandcode.ai；测试/镜像用）
---   whoami_override   凭据验证端点（默认 https://api.commandcode.ai/alpha/whoami）

local STUDIO = (mb.config and mb.config.studio_override) or "https://commandcode.ai"
local WHOAMI = (mb.config and mb.config.whoami_override) or "https://api.commandcode.ai/alpha/whoami"
local CALLBACK_PATH = "/callback"
local PREFERRED_PORT = 5959
local CLI_AUTH_FILE = "~/.commandcode/auth.json"

MB = {
  name = "auth-commandcode",
  version = "0.1.0",
  category = "auth",
  scopes = { "provider" },
  capabilities = { "auth" },
  -- mb.fs.read 白名单（仅此一份文件，精确匹配）
  fs_read_allow = { CLI_AUTH_FILE },
}

-- whoami 验证：合法 key 返回 { id, name }；非法/网络失败均 nil
local function whoami(key)
  local ok, r = pcall(mb.http.request, {
    method = "GET",
    url = WHOAMI,
    headers = { { "authorization", "Bearer " .. key }, { "accept", "application/json" } },
    timeout_ms = 10000,
  })
  if not ok or r.status ~= 200 or type(r.body) ~= "table" then return nil end
  local u = r.body.user
  if type(u) ~= "table" or u.id == nil or u.userName == nil then return nil end
  return { id = tostring(u.id), name = tostring(u.userName) }
end

-- 长期 key 令牌包：无 expires_at（core 永不触发刷新）
local function durable(key, account, name, source)
  return { access = key, account = account, email = name, source = source }
end

local function urlencode(s)
  return (s:gsub("([^%w%-%._~])", function(c)
    return string.format("%%%02X", string.byte(c))
  end))
end

local function urldecode(s)
  return (s:gsub("%%(%x%x)", function(h)
    return string.char(tonumber(h, 16))
  end))
end

-- 本地 CLI 凭据导入：文件缺失/无 key/whoami 失败各自显式区分
local function try_local_import()
  local ok, text = pcall(mb.fs.read, CLI_AUTH_FILE)
  if not ok or type(text) ~= "string" then return nil, "no-file" end
  local key = text:match('"apiKey"%s*:%s*"([^"]+)"')
  if not key or key == "" then return nil, "no-key" end
  local w = whoami(key)
  if not w then return nil, "whoami-failed" end
  return durable(key, w.id, w.name, "local-cli")
end

-- 手动粘贴解析：回调 JSON / 含 apiKey 的 URL / 裸 key。
-- URL 与 JSON 形态必须带匹配 state（防旧会话/他人回调混入）；裸 key 豁免（whoami 兜底）。
local function parse_paste(input, expected_state)
  local t = input:match("^%s*(.-)%s*$")
  if t == "" then return nil, "粘贴内容为空" end
  if t:sub(1, 1) == "{" then
    local key = t:match('"apiKey"%s*:%s*"([^"]+)"')
    local st = t:match('"state"%s*:%s*"([^"]+)"')
    local uid = t:match('"userId"%s*:%s*"([^"]+)"')
    if not key then return nil, "回调 JSON 解析失败（缺 apiKey）" end
    if st ~= expected_state then return nil, "state 不匹配（可能是旧会话的回调）" end
    return { key = key, account = uid, source = "manual" }
  end
  if t:match("^https?://") then
    local key = t:match("[?#&]apiKey=([^&]+)") or t:match("[?#&]api_key=([^&]+)")
      or t:match("[?#&]key=([^&]+)") or t:match("[?#&]token=([^&]+)")
    local st = t:match("[?#&]state=([^&]+)")
    if not key then return nil, "URL 中未找到 apiKey" end
    if st ~= expected_state then return nil, "state 不匹配（可能是旧会话的回调）" end
    return { key = urldecode(key), source = "manual" }
  end
  if t:find("%s") then return nil, "不是合法的 API key（含空白字符）" end
  return { key = t, source = "manual", raw = true }
end

function MB.auth_describe(ctx)
  return {
    kind = "callback",
    label = "Command Code 账户",
    supports_paste = true,
    instructions = "浏览器完成授权后自动回跳；也可手动粘贴回调信息或 API Key",
    sources = {
      { id = "local-cli", label = "导入本地 CLI 凭据" },
      { id = "browser", label = "浏览器授权登录" },
    },
    -- 登录成功后编排器据此建 provider（本插件的平台端点知识）
    provider = {
      key = "command-code-auth",
      label = "Command Code - Auth",
      protocol = "openai-chat",
      base_url = "https://api.commandcode.ai/provider/v1",
      dashboard_url = "https://commandcode.ai/studio/",
      note = "Command Code 账户登录（浏览器授权 / 本地 CLI 凭据导入）",
    },
  }
end

function MB.auth_begin(ctx)
  local source = ctx and ctx.source
  -- 本地导入：显式选择时失败即报错；缺省时失败回落浏览器
  if source ~= "browser" then
    local bundle, reason = try_local_import()
    if bundle then return { status = "done", bundle = bundle } end
    if source == "local-cli" then
      local msg = ({
        ["no-file"] = "未检测到本地 CLI 凭据（" .. CLI_AUTH_FILE .. " 不存在或不可读）",
        ["no-key"] = "本地 CLI 凭据文件缺少 apiKey",
        ["whoami-failed"] = "本地 CLI 凭据在线验证失败（whoami 未通过），请重新登录 CLI",
      })[reason] or ("本地导入失败: " .. tostring(reason))
      return { status = "error", message = msg }
    end
  end
  -- 浏览器回调流
  local state = mb.random.state()
  local l = mb.oauth.listen_callback({
    path = CALLBACK_PATH,
    origins = { STUDIO },
    preferred_port = PREFERRED_PORT,
  })
  local url = STUDIO .. "/studio/auth/cli?callback=" .. urlencode(l.url) .. "&state=" .. state
  local out = {
    verification_url = url,
    expires_in_secs = 120,
    handle = { listener = l.id, state = state },
  }
  if source ~= "browser" and source ~= "local-cli" then
    -- 缺省路径下本地导入失败曾发生：告知用户为何看到的是浏览器流
    out.notice = "未检测到可用的本地 CLI 凭据，已改用浏览器登录"
  end
  return out
end

function MB.auth_poll(ctx, handle, paste)
  -- 手动粘贴优先（回调 JSON / URL / 裸 key）
  if type(paste) == "string" and paste ~= "" then
    local parsed, err = parse_paste(paste, handle.state)
    if not parsed then
      return { status = "pending", message = err }
    end
    if parsed.raw then
      local w = whoami(parsed.key)
      if not w then
        return { status = "pending", message = "API Key 验证失败，请检查后重试" }
      end
      mb.oauth.callback_close(handle.listener)
      return { status = "done", bundle = durable(parsed.key, w.id, w.name, "manual") }
    end
    mb.oauth.callback_close(handle.listener)
    return { status = "done", bundle = durable(parsed.key, parsed.account, nil, parsed.source) }
  end
  -- 浏览器回调（1s 心跳，编排器掌握总超时与取消）
  local r = mb.oauth.callback_await(handle.listener, 1000)
  if not r then return { status = "pending" } end
  mb.oauth.callback_close(handle.listener)
  if r.state ~= handle.state then
    return { status = "error", message = "state 不匹配（可能是旧会话的回调）" }
  end
  if not r.apiKey or r.apiKey == "" then
    return { status = "error", message = "回调缺少 apiKey" }
  end
  return { status = "done", bundle = durable(r.apiKey, r.userId, r.userName, "oauth") }
end

function MB.auth_refresh(ctx, bundle)
  error("Command Code 长期 Key 无刷新端点，请重新登录")
end

function MB.auth_headers(ctx, bundle)
  return { { "authorization", "Bearer " .. bundle.access } }
end
