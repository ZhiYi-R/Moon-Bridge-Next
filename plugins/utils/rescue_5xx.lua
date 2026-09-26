-- rescue_5xx：上游瞬时 5xx / 429 自愈——命中状态码就让网关延迟后重发本次请求。
--
-- 场景：单端点 provider（如 api.commandcode.ai 这类只有一个端点的上游）偶发 520/502 时，
-- 端点故障转移循环里没有下一个端点可切换，错误直接冒泡给客户端（Agent 子代理当场死亡）。
-- 本插件挂在「上游响应报文层」的**错误路径**上，把它变成一个「延迟后重试」决策，
-- 由宿主原生重发整条端点链——协议翻译、流式、trace 机制全部照旧；插件不自建转发
-- （用 mb.http.request 手工搬报文会绕过协议转换与流式）。
--
-- 延迟退避：第 n 次重试等 min(base_delay_ms * 2^(n-1), max_delay_ms)。
-- 计数按 ctx.request_id 键控（同一请求的每一轮失败各触发一次本钩子），FIFO 容量 256 防泄漏。
--
-- 只返回 { action = "retry", delay_ms = N } 或 nil（放行原始错误）：
-- **不 short_circuit、不改 body**——本地代答会让流式请求的 SSE 语义当场破坏
-- （客户端等的是一个事件流，不是一个 JSON 对象）。
--
-- 硬防护在宿主侧（config.toml，插件无法绕过）：`pluginRetryMax`（默认 8，0 = 禁用插件
-- 重试）与 `pluginRetryDelayCapMs`（默认 30000）兜底，插件返回多少次 retry 都不会死循环，
-- 也不会把单次等待睡到不可接受的长度。

MB = {
  name = "rescue_5xx",
  version = "1.0.0",
  category = "core",
  scopes = { "global", "provider" },
  capabilities = { "raw_response" },
  entry = "on_upstream_response_raw",
  config_schema = {
    type = "object",
    properties = {
      statuses = {
        type = "array",
        items = { type = "number" },
        title = "触发重试的上游状态码",
        default = { 429, 500, 502, 503, 504, 520, 521, 522, 524, 529 },
      },
      max_retries = {
        type = "number",
        title = "单请求最大重试次数（0 = 只观测不重试）",
        default = 5,
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

local DEFAULT_STATUSES = { 429, 500, 502, 503, 504, 520, 521, 522, 524, 529 }
local DEFAULT_MAX_RETRIES = 5
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
  local cfg = { statuses = {} }
  if type(c.statuses) == "table" then
    for _, s in ipairs(c.statuses) do
      if type(s) == "number" then
        cfg.statuses[math.floor(s)] = true
      end
    end
  end
  if next(cfg.statuses) == nil then
    for _, s in ipairs(DEFAULT_STATUSES) do
      cfg.statuses[s] = true
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

-- msg: { stage = "upstream_response", status, headers, body, provider, ... }
function MB.on_upstream_response_raw(ctx, msg)
  local cfg = config()
  local status = tonumber(msg.status) or 0
  local id = ctx.request_id

  if not cfg.statuses[status] then
    -- 未命中：2xx 成功或未列出的 4xx——该请求已无需重试，顺手释放计数
    forget(id)
    return nil
  end

  local n = (ATTEMPTS[id] or 0) + 1
  if n > cfg.max_retries then
    -- 自愿封顶耗尽：清掉计数并放行原始错误（宿主另有 pluginRetryMax 硬上限）
    forget(id)
    mb.log.warn(string.format(
      "[rescue_5xx] %s 上游 HTTP %d 连试 %d 次仍未成功，放行原始错误",
      ctx.model_alias or "-", status, cfg.max_retries))
    return nil
  end

  -- 指数退避：2^(n-1) 在 Lua 里是浮点，math.floor 归一回整数毫秒（宿主按整数解析）
  local delay = math.floor(math.min(cfg.base_delay_ms * 2 ^ (n - 1), cfg.max_delay_ms))
  remember(id)
  ATTEMPTS[id] = n
  mb.log.warn(string.format(
    "[rescue_5xx] %s 上游 HTTP %d，第 %d/%d 次重试，%dms 后重发",
    ctx.model_alias or "-", status, n, cfg.max_retries, delay))
  return { action = "retry", delay_ms = delay }
end
