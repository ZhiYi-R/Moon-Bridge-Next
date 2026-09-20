// 余额看板内置脚本模板：按调研确认的各服务真实接口编写（2026-09 核对）。
//
// 每个模板都是可直接运行的完整脚本（首行必须是 `MB = {}`），只按单 key 编写
// （多 key 由引擎逐 key 执行）。沙箱无 os/io 库：时间戳只能透传 ISO 字符串
// 或返回 unix 秒数字（引擎会把数字归一成字符串，前端再格式化为时间）。
//
// 接口依据摘要：
// - new-api：GET {base}/api/user/self → data.quota / 500000 作余额（默认 $）
// - DeepSeek：GET api.deepseek.com/user/balance → balance_infos[]（金额为字符串）
// - Moonshot：GET api.moonshot.cn/v1/users/me/balance → data.available/voucher/cash_balance
// - SiliconFlow：GET api.siliconflow.cn/v1/user/info → data.balance/chargeBalance/totalBalance
// - OpenRouter：GET openrouter.ai/api/v1/credits + /api/v1/key → credits 相减得余额
// - 智谱 Coding Plan：GET open.bigmodel.cn/api/monitor/usage/quota/limit（裸 key 鉴权）
//   → data.limits[].{type, percentage, nextResetTime(ms)}
// - Kimi Coding Plan：GET api.kimi.com/coding/v1/usages → usages.limit_5h/limit_7d.{used_ratio,reset_time}
// - CommandCode：GET api.commandcode.ai/alpha/billing/credits → credits.monthlyCredits +
//   windowLimits.{fiveHour,weekly}.{used,cap,resetAt(ms)}
// - Claude Code 订阅：GET api.anthropic.com/api/oauth/usage（OAuth token + 必需头）
//   → five_hour/seven_day.{utilization, resets_at(ISO)}

/** 新建或编辑卡片时可选的脚本模板。 */
export interface BalanceTemplate {
  id: string;
  label: string;
  /** 模板适用面与注意事项（选中后展示在表单里）。 */
  description: string;
  /** 建议的查询 URL（选模板时填入表单，用户可改）；不填时保留当前值。 */
  baseUrl?: string;
  /** 建议的额外参数 JSON 文本。 */
  extraText?: string;
  /** 建议的查询间隔（秒）。 */
  intervalSecs?: number;
  script: string;
}

const GENERIC_PERCENT = `MB = {}
-- 通用百分比模板：以 ctx.key 鉴权 GET {base_url}/balance，组装百分比配额

function MB.query(ctx)
  local r = mb.http.request({
    method = "GET",
    url = ctx.base_url .. "/balance",
    headers = { { "authorization", "Bearer " .. ctx.key } },
    timeout_ms = 10000,
  })
  if r.status ~= 200 then
    return { status = "error", message = "上游返回 HTTP " .. tostring(r.status) }
  end
  local d = r.body or {}
  return {
    status = "ok",
    summary = d.summary,
    quotas = {
      { label = "5 小时窗口", used_percent = d.used_percent, reset_at = d.reset_at },
      { label = "周额度", used_percent = d.weekly_used_percent },
    },
  }
end
`;

const GENERIC_AMOUNT = `MB = {}
-- 通用金额模板：读取已用/剩余金额；币种从卡片「额外参数 JSON」的 unit 取（默认 ¥）

function MB.query(ctx)
  local r = mb.http.request({
    method = "GET",
    url = ctx.base_url .. "/billing/balance",
    headers = { { "authorization", "Bearer " .. ctx.key } },
    timeout_ms = 10000,
  })
  if r.status ~= 200 then
    return { status = "error", message = "上游返回 HTTP " .. tostring(r.status) }
  end
  local d = r.body or {}
  local unit = (ctx.extra and ctx.extra.unit) or "¥"
  local left = d.left_amount or d.balance
  return {
    status = "ok",
    summary = left and ("剩余 " .. tostring(left) .. unit) or nil,
    quotas = {
      { label = "余额", unit = unit, used_amount = d.used_amount, left_amount = left },
    },
  }
end
`;

const NEW_API = `MB = {}

local function number(value)
  if type(value) ~= "number" and type(value) ~= "string" then return nil end
  local n = tonumber(value)
  if not n or n ~= n or n == math.huge or n == -math.huge then return nil end
  return n
end

function MB.query(ctx)
  local base = (ctx.base_url or ""):match("^%s*(.-)%s*$"):gsub("/+$", "")
  if base == "" then
    return { status = "error", message = "请填写查询 URL（站点根地址，不带 /v1）" }
  end
  local extra = type(ctx.extra) == "table" and ctx.extra or {}
  local qpu = number(extra.quota_per_unit == nil and 500000 or extra.quota_per_unit)
  if not qpu or qpu <= 0 then
    return { status = "error", message = "quota_per_unit 必须是大于 0 的有限数字" }
  end
  local r = mb.http.request({
    method = "GET",
    url = base .. "/api/user/self",
    headers = { { "authorization", "Bearer " .. ctx.key } },
    timeout_ms = 10000,
  })
  if r.status ~= 200 then
    return { status = "error", message = "站点返回 HTTP " .. tostring(r.status) }
  end
  local body = type(r.body) == "table" and r.body or {}
  if body.success == false or body.error then
    return { status = "error", message = tostring(body.message or body.error or "账户查询失败") }
  end
  local d = type(body.data) == "table" and body.data or {}
  local quota = number(d.quota)
  if not quota then
    return { status = "error", message = "响应缺少有效的 data.quota" }
  end
  if quota <= 0 then
    return { status = "ok", summary = "无额度记录", quotas = {} }
  end
  return {
    status = "ok",
    quotas = {
      { label = "额度", unit = extra.unit or "$", left_amount = quota / qpu },
    },
  }
end
`;

const DEEPSEEK = `MB = {}
-- DeepSeek 官方：GET https://api.deepseek.com/user/balance
-- 返回多币种 balance_infos 数组（金额是字符串），逐币种各出一条配额

function MB.query(ctx)
  local r = mb.http.request({
    method = "GET",
    url = string.gsub(ctx.base_url ~= "" and ctx.base_url or "https://api.deepseek.com", "/+$", "") .. "/user/balance",
    headers = { { "authorization", "Bearer " .. ctx.key } },
    timeout_ms = 10000,
  })
  if r.status ~= 200 then
    return { status = "error", message = "上游返回 HTTP " .. tostring(r.status) }
  end
  local infos = (r.body or {}).balance_infos
  if not infos or #infos == 0 then
    return { status = "error", message = "响应缺少 balance_infos" }
  end
  local quotas = {}
  for _, info in ipairs(infos) do
    local cur = info.currency or "?"
    local unit = cur == "CNY" and "¥" or (cur == "USD" and "$" or cur)
    table.insert(quotas, {
      label = "余额（" .. cur .. "）",
      unit = unit,
      left_amount = tonumber(info.total_balance),
    })
  end
  return { status = "ok", summary = tostring(#infos) .. " 个币种账户", quotas = quotas }
end
`;

const MOONSHOT = `MB = {}
-- Moonshot / Kimi 开放平台：GET {base}/v1/users/me/balance
-- 国内站 api.moonshot.cn，国际站 api.moonshot.ai（两把 key 不通用）；
-- available_balance = voucher_balance + cash_balance（人民币元），可用 ≤ 0 即欠费停机

function MB.query(ctx)
  local base = ctx.base_url ~= "" and ctx.base_url or "https://api.moonshot.cn"
  local r = mb.http.request({
    method = "GET",
    url = base .. "/v1/users/me/balance",
    headers = { { "authorization", "Bearer " .. ctx.key } },
    timeout_ms = 10000,
  })
  if r.status ~= 200 then
    return { status = "error", message = "上游返回 HTTP " .. tostring(r.status) }
  end
  local body = r.body or {}
  local d = body.data
  if body.code ~= 0 or not d then
    return { status = "error", message = "查询失败：" .. tostring(body.message or body.scode or "未知错误") }
  end
  return {
    status = "ok",
    summary = "可用余额 ¥" .. tostring(d.available_balance),
    quotas = {
      { label = "可用余额", unit = "¥", left_amount = d.available_balance },
      { label = "充值余额", unit = "¥", left_amount = d.cash_balance },
      { label = "代金券", unit = "¥", left_amount = d.voucher_balance },
    },
  }
end
`;

const SILICONFLOW = `MB = {}
-- SiliconFlow 硅基流动：GET {base}/v1/user/info（国内 .cn / 国际 .com）
-- data.totalBalance ≈ chargeBalance（充值）+ balance（赠金），金额为字符串

function MB.query(ctx)
  local base = ctx.base_url ~= "" and ctx.base_url or "https://api.siliconflow.cn"
  local r = mb.http.request({
    method = "GET",
    url = base .. "/v1/user/info",
    headers = { { "authorization", "Bearer " .. ctx.key } },
    timeout_ms = 10000,
  })
  if r.status ~= 200 then
    return { status = "error", message = "上游返回 HTTP " .. tostring(r.status) }
  end
  local d = (r.body or {}).data
  if not d then
    return { status = "error", message = "响应缺少 data 字段" }
  end
  return {
    status = "ok",
    summary = "总余额 ¥" .. tostring(d.totalBalance),
    quotas = {
      { label = "总余额", unit = "¥", left_amount = tonumber(d.totalBalance) },
      { label = "充值余额", unit = "¥", left_amount = tonumber(d.chargeBalance) },
      { label = "赠金余额", unit = "¥", left_amount = tonumber(d.balance) },
    },
  }
end
`;

const OPENROUTER = `MB = {}
-- OpenRouter：/api/v1/credits（账户累计购入/已用）+ /api/v1/key（本 key 限额）
-- 余额 = total_credits - total_usage；key 设了 limit 时附带限额百分比条

function MB.query(ctx)
  local base = string.gsub(ctx.base_url ~= "" and ctx.base_url or "https://openrouter.ai", "/+$", "")
  local h = { { "authorization", "Bearer " .. ctx.key } }
  local r = mb.http.request({
    method = "GET",
    url = base .. "/api/v1/credits",
    headers = h,
    timeout_ms = 10000,
  })
  if r.status ~= 200 then
    return { status = "error", message = "credits 接口返回 HTTP " .. tostring(r.status) }
  end
  local cd = (r.body or {}).data
  if not cd then
    return { status = "error", message = "响应缺少 data 字段" }
  end
  local left = (cd.total_credits or 0) - (cd.total_usage or 0)
  local quotas = {
    { label = "账户余额", unit = "$", used_amount = cd.total_usage, left_amount = left },
  }
  -- 本 key 限额（未设限时 limit 为 null，跳过）
  local k = mb.http.request({
    method = "GET",
    url = base .. "/api/v1/key",
    headers = h,
    timeout_ms = 10000,
  })
  local kd = k.status == 200 and (k.body or {}).data or nil
  if kd and kd.limit then
    table.insert(quotas, {
      label = "本 key 限额",
      used_percent = (kd.usage or 0) / kd.limit * 100,
      unit = "$",
      used_amount = kd.usage,
      left_amount = kd.limit_remaining,
    })
  end
  return {
    status = "ok",
    summary = string.format("账户剩余 %.2f$", left),
    quotas = quotas,
  }
end
`;

const ZHIPU_GLM = `MB = {}
-- 智谱 GLM Coding Plan：GET {base}/api/monitor/usage/quota/limit
-- 国内 open.bigmodel.cn / 国际 api.z.ai；鉴权用裸 key（不带 Bearer），失败回退 Bearer。
-- data.limits[] 的 type：TIME_LIMIT = 5 小时窗口，TOKENS_LIMIT = token 周窗口；
-- percentage 是已用百分比；nextResetTime 是毫秒时间戳（reset_at 也可以给 unix 秒数字）。

function MB.query(ctx)
  local base = ctx.base_url ~= "" and ctx.base_url or "https://open.bigmodel.cn"
  local function call(auth)
    return mb.http.request({
      method = "GET",
      url = base .. "/api/monitor/usage/quota/limit",
      headers = { { "authorization", auth } },
      timeout_ms = 10000,
    })
  end
  local r = call(ctx.key)
  if r.status ~= 200 then
    r = call("Bearer " .. ctx.key)
  end
  if r.status ~= 200 then
    return { status = "error", message = "上游返回 HTTP " .. tostring(r.status) }
  end
  local data = (r.body or {}).data
  local limits = data and data.limits
  if not limits or #limits == 0 then
    return { status = "error", message = "响应缺少 data.limits（该 key 可能不是 Coding Plan）" }
  end
  local quotas = {}
  for _, w in ipairs(limits) do
    local label = w.type == "TIME_LIMIT" and "5 小时窗口"
      or (w.type == "TOKENS_LIMIT" and "周窗口" or (w.type or "配额"))
    table.insert(quotas, {
      label = label,
      used_percent = w.percentage,
      reset_at = w.nextResetTime and math.floor(w.nextResetTime / 1000) or nil,
    })
  end
  return {
    status = "ok",
    summary = data.level and ("套餐档位：" .. tostring(data.level)) or nil,
    quotas = quotas,
  }
end
`;

const KIMI_CODING = `MB = {}

function MB.query(ctx)
  local base = ctx.base_url ~= "" and ctx.base_url or "https://api.kimi.com"
  local r = mb.http.request({
    method = "GET",
    url = string.gsub(base, "/+$", "") .. "/coding/v1/usages",
    headers = {
      { "authorization", "Bearer " .. ctx.key },
      { "user-agent", "cli" },
    },
    timeout_ms = 10000,
  })
  if r.status ~= 200 then
    return { status = "error", message = "上游返回 HTTP " .. tostring(r.status) }
  end
  local body = type(r.body) == "table" and r.body or {}
  local usages = type(body.usages) == "table" and body.usages or {}
  local quotas = {}
  for _, item in ipairs({ { "limit_5h", "5 小时" }, { "limit_7d", "Weekly" } }) do
    local w = usages[item[1]]
    local value = type(w) == "table" and w.used_ratio or nil
    local ratio = (type(value) == "number" or type(value) == "string") and tonumber(value) or nil
    if ratio and ratio == ratio and ratio >= 0 and ratio < math.huge and ratio * 100 < math.huge then
      table.insert(quotas, {
        label = item[2],
        used_percent = ratio * 100,
        reset_at = w.reset_time,
      })
    end
  end
  if #quotas == 0 then
    return { status = "error", message = "响应缺少有效的 usages.limit_5h / limit_7d.used_ratio" }
  end
  return { status = "ok", quotas = quotas }
end
`;

const COMMANDCODE = `MB = {}

local function round2(value)
  return math.floor(value * 100 + 0.5) / 100
end

function MB.query(ctx)
  local extra = ctx.extra or {}
  local monthly_cap = tonumber(extra.monthly_cap or 70)
  if not monthly_cap or monthly_cap <= 0 then
    return { status = "error", message = "monthly_cap 必须是大于 0 的数字" }
  end
  local unit = extra.unit or ""
  local url = ctx.base_url ~= "" and ctx.base_url or "https://api.commandcode.ai/alpha/billing/credits"
  local r = mb.http.request({
    method = "GET",
    url = url,
    headers = {
      { "authorization", "Bearer " .. ctx.key },
      { "content-type", "application/json" },
      { "user-agent", "cli" },
    },
    timeout_ms = 10000,
  })
  if r.status ~= 200 then
    return { status = "error", message = "上游返回 HTTP " .. tostring(r.status) }
  end
  local body = type(r.body) == "table" and r.body or {}
  local credits = type(body.credits) == "table" and body.credits or {}
  local monthly_left = tonumber(credits.monthlyCredits)
  local quotas = {}
  if monthly_left then
    table.insert(quotas, {
      label = "Monthly",
      unit = unit,
      used_amount = round2(monthly_cap - monthly_left),
      left_amount = round2(math.max(monthly_left, 0)),
    })
  end
  local windows = type(body.windowLimits) == "table" and body.windowLimits or {}
  for _, item in ipairs({ { "fiveHour", "5H" }, { "weekly", "Weekly" } }) do
    local w = windows[item[1]]
    if w ~= nil then
      local used = type(w) == "table" and tonumber(w.used) or nil
      local cap = type(w) == "table" and tonumber(w.cap) or nil
      if not used or not cap or cap < 0 then
        return { status = "error", message = "响应缺少有效的 windowLimits." .. item[1] .. ".used/cap" }
      end
      local reset = tonumber(w.resetAt)
      table.insert(quotas, {
        label = item[2],
        unit = unit,
        used_amount = round2(used),
        left_amount = round2(math.max(cap - used, 0)),
        reset_at = reset and math.floor(reset / 1000) or nil,
      })
    end
  end
  if #quotas == 0 then
    return { status = "error", message = "响应缺少 credits.monthlyCredits / windowLimits" }
  end
  return {
    status = "ok",
    summary = monthly_left and string.format("月度剩余 %.2f%s / %g%s", math.max(monthly_left, 0), unit, monthly_cap, unit) or nil,
    quotas = quotas,
  }
end
`;

const CLAUDE_CODE = `MB = {}
-- Claude Code 订阅（OAuth）：GET https://api.anthropic.com/api/oauth/usage
-- 注意：上游服务里要放 Claude Code 的 OAuth access token（不是 sk-ant API key）；
-- anthropic-beta 与 claude-code 的 User-Agent 是必需头，缺失会被 429。
-- utilization 是已用百分比，resets_at 是 ISO 时间字符串（原样透传展示）。

function MB.query(ctx)
  local r = mb.http.request({
    method = "GET",
    url = string.gsub(ctx.base_url ~= "" and ctx.base_url or "https://api.anthropic.com", "/+$", "") .. "/api/oauth/usage",
    headers = {
      { "authorization", "Bearer " .. ctx.key },
      { "anthropic-beta", "oauth-2025-04-20" },
      { "user-agent", "claude-code/2.0" },
    },
    timeout_ms = 10000,
  })
  if r.status ~= 200 then
    return { status = "error", message = "上游返回 HTTP " .. tostring(r.status)
      .. "（429 多为限流，调大查询间隔；401 检查是否 OAuth token）" }
  end
  local body = r.body or {}
  local quotas = {}
  local function window(label, w)
    if not w or not w.utilization then return end
    table.insert(quotas, {
      label = label,
      used_percent = w.utilization,
      reset_at = w.resets_at,
    })
  end
  window("5 小时", body.five_hour)
  window("7 天", body.seven_day)
  window("7 天（Opus）", body.seven_day_opus)
  window("7 天（Sonnet）", body.seven_day_sonnet)
  if #quotas == 0 then
    return { status = "error", message = "响应缺少窗口数据" }
  end
  return { status = "ok", quotas = quotas }
end
`;

const SUB2API = `MB = {}

function MB.query(ctx)
  local base = (ctx.base_url or ""):match("^%s*(.-)%s*$")
  base = base:gsub("/+$", ""):gsub("/v1$", "")
  if base == "" then
    return { status = "error", message = "请在卡片上填写查询 URL（Sub2API 站点地址）" }
  end
  if not ctx.key or ctx.key == "" then
    return { status = "error", message = "请填写 Sub2API API Key" }
  end
  local r = mb.http.request({
    method = "GET",
    url = base .. "/v1/usage",
    headers = { { "authorization", "Bearer " .. ctx.key } },
    timeout_ms = 10000,
  })
  if r.status ~= 200 then
    local message = "查询失败，HTTP " .. tostring(r.status)
    if r.status == 401 then
      message = message .. "：API Key 无效或未被接受"
    elseif r.status == 403 then
      message = message .. "：访问被拒绝，请检查 Key 状态及访问限制"
    elseif r.status == 404 then
      message = message .. "：请检查站点地址及是否支持 /v1/usage"
    end
    return { status = "error", message = message }
  end
  local d = r.body
  if type(d) ~= "table" then
    return { status = "error", message = "接口未返回有效的 JSON 对象" }
  end
  if d.isValid == false then
    return { status = "error", message = "接口返回 Key 不可用" }
  end
  local quotas = {}
  local function add(label, amount)
    local value = tonumber(amount)
    if value ~= nil then
      quotas[#quotas + 1] = { label = label, unit = "$", left_amount = value }
      return true
    end
    return false
  end
  local has_amount = false
  if type(d.quota) == "table" then
    has_amount = add("Key 剩余额度", d.quota.remaining)
  end
  if not has_amount then
    has_amount = add("钱包余额", d.balance)
  end
  if not has_amount then
    local label = "剩余额度"
    if d.mode == "quota_limited" then
      label = "Key 剩余额度"
    elseif d.planName == "钱包余额" then
      label = "钱包余额"
    elseif d.planName then
      label = tostring(d.planName) .. " 剩余额度"
    end
    add(label, d.remaining)
  end
  local windows = { ["5h"] = "5 小时", ["1d"] = "1 天", ["7d"] = "7 天" }
  if type(d.rate_limits) == "table" then
    for _, limit in ipairs(d.rate_limits) do
      if type(limit) == "table" then
        local window = tostring(limit.window or "")
        add((windows[window] or window) .. "周期剩余额度", limit.remaining)
      end
    end
  end
  if #quotas == 0 then
    return {
      status = "error",
      message = "响应未包含余额或剩余额度，可能是订阅信息不可用或站点版本不兼容",
    }
  end
  return { status = "ok", quotas = quotas }
end
`;

/** 内置模板清单（新建卡片时的选择顺序即数组顺序）。 */
export const BALANCE_TEMPLATES: BalanceTemplate[] = [
  {
    id: "generic-percent",
    label: "通用百分比",
    description: "最通用的一版：Bearer 鉴权 GET 一个地址，把返回字段映射成百分比配额。其他服务都不匹配时从它改起。",
    script: GENERIC_PERCENT,
  },
  {
    id: "generic-amount",
    label: "通用金额",
    description: "金额模板：返回「消耗/余额」金额字段，币种走额外参数 unit（默认 ¥）。",
    extraText: '{\n  "unit": "¥"\n}',
    script: GENERIC_AMOUNT,
  },
  {
    id: "new-api",
    label: "new-api 中转站",
    description:
      "Bearer 鉴权 GET {URL}/api/user/self，将 data.quota / 500000 显示为余额（默认 $）；quota ≤ 0 显示「无额度记录」。请使用能访问账户接口的凭据，查询 URL 填站点根地址，不带 /v1。换算和单位可通过额外参数调整。",
    baseUrl: "",
    extraText: '{\n  "quota_per_unit": 500000,\n  "unit": "$"\n}',
    script: NEW_API,
  },
  {
    id: "sub2api",
    label: "Sub2API",
    description: "Bearer 鉴权 GET /v1/usage：优先显示 Key 剩余额度，其次钱包余额或套餐剩余额度，并附带 5 小时 / 1 天 / 7 天周期余额（美元）。查询 URL 填站点根地址或以 /v1 结尾的地址。",
    baseUrl: "",
    extraText: "{}",
    script: SUB2API,
  },
  {
    id: "deepseek",
    label: "DeepSeek",
    description: "DeepSeek 官方余额接口，逐币种账户各出一条配额（金额为字符串，自动转数字）。默认 api.deepseek.com，可修改查询 URL 的根地址。",
    baseUrl: "https://api.deepseek.com",
    script: DEEPSEEK,
  },
  {
    id: "moonshot",
    label: "Moonshot / Kimi 开放平台",
    description: "开放平台账户余额：可用 = 充值 + 代金券（人民币元）。默认国内站 api.moonshot.cn，国际站把查询 URL 改为 https://api.moonshot.ai。注意与 Kimi Coding Plan 是两个产品、key 不通用。",
    baseUrl: "https://api.moonshot.cn",
    script: MOONSHOT,
  },
  {
    id: "siliconflow",
    label: "SiliconFlow 硅基流动",
    description: "总余额 / 充值 / 赠金三条配额（人民币元）。默认国内站 api.siliconflow.cn，国际站把查询 URL 改为 https://api.siliconflow.com。",
    baseUrl: "https://api.siliconflow.cn",
    script: SILICONFLOW,
  },
  {
    id: "openrouter",
    label: "OpenRouter",
    description: "账户余额（累计购入 − 累计已用，美元）；key 设了限额时附带限额百分比条。默认 openrouter.ai，可修改查询 URL 的根地址。",
    baseUrl: "https://openrouter.ai",
    script: OPENROUTER,
  },
  {
    id: "zhipu-glm",
    label: "智谱 GLM Coding Plan",
    description: "GLM Coding Plan 窗口配额（5 小时 / 周窗口的已用百分比 + 重置时间）。默认国内 open.bigmodel.cn，国际站把查询 URL 改为 https://api.z.ai。鉴权自动尝试裸 key 与 Bearer 两种形式。",
    baseUrl: "https://open.bigmodel.cn",
    script: ZHIPU_GLM,
  },
  {
    id: "kimi-coding",
    label: "Kimi Coding Plan",
    description: "Kimi 编程订阅：读取 usages.limit_5h / limit_7d 的 used_ratio 与 reset_time，任一有效窗口即可显示，两者均无效时报错。无需配置额度上限，查询 URL 填 API 根地址。",
    baseUrl: "https://api.kimi.com",
    extraText: "{}",
    script: KIMI_CODING,
  },
  {
    id: "commandcode",
    label: "CommandCode",
    description: "月度剩余额度 + 5H / Weekly 金额窗口；窗口上限取接口 cap，月上限默认 70，可在额外参数 monthly_cap 修改。查询 URL 是完整接口地址；unit 默认空，可自行填写。",
    baseUrl: "https://api.commandcode.ai/alpha/billing/credits",
    extraText: '{\n  "monthly_cap": 70,\n  "unit": ""\n}',
    script: COMMANDCODE,
  },
  {
    id: "claude-code",
    label: "Claude Code 订阅",
    description: "Claude Code 订阅窗口配额（5 小时 / 7 天已用百分比 + 重置时间）。注意：上游服务里要放 Claude Code 的 OAuth access token（不是 sk-ant API key）；接口有限流，建议间隔 ≥ 300 秒。查询 URL 为 API 根地址。",
    baseUrl: "https://api.anthropic.com",
    intervalSecs: 300,
    script: CLAUDE_CODE,
  },
];

/** 新建卡片的默认脚本（通用百分比模板）。 */
export const DEFAULT_BALANCE_SCRIPT = GENERIC_PERCENT;
