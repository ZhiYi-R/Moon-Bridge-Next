-- rescue_fake_200：流式请求零产出失败自动重试插件。
--
-- 当上游返回 HTTP 200 但在产生任何内容块前即发送带内错误帧（如 `response.failed`）或异常关闭流时，
-- 网关会以 `msg.error == true` 且 `msg.status == 200` 触发 `on_upstream_response_raw`。
-- 本插件在此路径上返回 `{ action = "retry", delay_ms = N }`，由网关按指数退避（或错误附带的
-- `[retry-after=Ns]` 提示）延迟后重发请求。
--
-- 重试计数按 `ctx.request_id` 隔离（FIFO 容量 256），并受网关 `pluginRetryMax` 与
-- `pluginRetryDelayCapMs` 配置上限约束。

MB = {
  name = "rescue_fake_200",
  version = "1.0.0",
  category = "core",
  scopes = { "global", "provider" },
  capabilities = { "raw_response" },
  entry = "on_upstream_response_raw",
  config_schema = {
    type = "object",
    properties = {
      match_any = {
        type = "array",
        items = { type = "string" },
        title = "错误文本匹配（子串，任一命中即重试；空表 = 全部重试）",
        default = {},
      },
      max_retries = {
        type = "number",
        title = "单请求最大重试次数（0 = 只观测不重试）",
        default = 3,
      },
      base_delay_ms = {
        type = "number",
        title = "首次重试延迟（毫秒，指数退避基数）",
        default = 500,
      },
      max_delay_ms = {
        type = "number",
        title = "单次重试延迟上限（毫秒）",
        default = 8000,
      },
    },
  },
}

local DEFAULT_MAX_RETRIES = 3
local DEFAULT_BASE_DELAY_MS = 500
local DEFAULT_MAX_DELAY_MS = 8000
-- 计数表容量上限：按 request_id 键控的表没有天然回收点，必须有界（防长跑泄漏）
local MAX_TRACKED = 256

local ATTEMPTS = {} -- request_id -> 已重试次数
local ORDER = {} -- request_id 插入顺序（FIFO 淘汰用）
local CFG = nil -- 解析后的配置（首次调用时算一次）

local function cfg_int(v, default, min)
  if type(v) == "number" and v >= min then
    return math.floor(v)
  end
  return default
end

-- 解析配置并缓存。懒解析 + 逐项回落默认值：配置里有脏值时只影响本钩子，
-- 绝不让插件加载失败——救援插件自己挂掉比不装它更糟。
local function config()
  if CFG then
    return CFG
  end
  local c = mb.config or {}
  local cfg = { match_any = {} }
  if type(c.match_any) == "table" then
    for _, s in ipairs(c.match_any) do
      if type(s) == "string" and s ~= "" then
        cfg.match_any[#cfg.match_any + 1] = s
      end
    end
  end
  cfg.max_retries = cfg_int(c.max_retries, DEFAULT_MAX_RETRIES, 0)
  cfg.base_delay_ms = cfg_int(c.base_delay_ms, DEFAULT_BASE_DELAY_MS, 1)
  cfg.max_delay_ms = cfg_int(c.max_delay_ms, DEFAULT_MAX_DELAY_MS, 1)
  CFG = cfg
  return CFG
end

local function remember(id)
  if ATTEMPTS[id] ~= nil then
    return
  end
  ORDER[#ORDER + 1] = id
  while #ORDER > MAX_TRACKED do
    ATTEMPTS[table.remove(ORDER, 1)] = nil
  end
end

local function forget(id)
  if ATTEMPTS[id] == nil then
    return
  end
  ATTEMPTS[id] = nil
  for i = #ORDER, 1, -1 do
    if ORDER[i] == id then
      table.remove(ORDER, i)
    end
  end
end

local function body_text(msg)
  if type(msg.body) == "string" then
    return msg.body
  end
  return ""
end

-- msg: { stage = "upstream_response", status, headers, body, provider, error, ... }
--   流内零产出失败由网关注入 msg.error == true 且 msg.status == 200（只读）。
function MB.on_upstream_response_raw(ctx, msg)
  local cfg = config()
  local id = ctx.request_id
  local status = tonumber(msg.status) or 0

  -- 只处理「假 200」：error 标记 + 200 状态码。真非 2xx 归 rescue_5xx 管；
  -- 成功路径顺手释放可能残留的计数。
  if not msg.error or status ~= 200 then
    forget(id)
    return nil
  end

  local text = body_text(msg)
  if #cfg.match_any > 0 then
    local hit = false
    for _, pat in ipairs(cfg.match_any) do
      if text:find(pat, 1, true) then
        hit = true
        break
      end
    end
    if not hit then
      forget(id)
      return nil
    end
  end

  local n = (ATTEMPTS[id] or 0) + 1
  if n > cfg.max_retries then
    -- 自愿封顶耗尽：清掉计数并放行原始错误（宿主另有 pluginRetryMax 硬上限）
    forget(id)
    mb.log.warn(string.format(
      "[rescue_fake_200] %s 流内零产出失败连试 %d 次仍未成功，放行原始错误",
      ctx.model_alias or "-", cfg.max_retries))
    return nil
  end

  -- 上游若带了限流建议等待时长（`[retry-after=Ns]`，由 decode 从错误帧
  -- headers.Retry-After 提取），优先听它的——容量窗口（Codex turn 限流实测 50s+）
  -- 里固定退避只会白烧重试次数。仍受宿主 pluginRetryDelayCapMs 硬封顶。
  local delay
  local ra = tonumber(text:match("%[retry%-after=(%d+)%]"))
  if ra and ra > 0 then
    delay = ra * 1000
  else
    -- 指数退避：2^(n-1) 在 Lua 里是浮点，math.floor 归一回整数毫秒（宿主按整数解析）
    delay = math.floor(math.min(cfg.base_delay_ms * 2 ^ (n - 1), cfg.max_delay_ms))
  end
  remember(id)
  ATTEMPTS[id] = n
  mb.log.warn(string.format(
    "[rescue_fake_200] %s 上游流内宣告失败（%s），第 %d/%d 次重试，%dms 后重发",
    ctx.model_alias or "-", text:sub(1, 120), n, cfg.max_retries, delay))
  return { action = "retry", delay_ms = delay }
end
