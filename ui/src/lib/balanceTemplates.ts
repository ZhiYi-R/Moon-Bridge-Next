// 余额看板内置脚本模板：按调研确认的各服务真实接口编写（2026-09 口径）。
//
// 每个模板都是可直接运行的完整脚本（首行必须是 `MB = {}`），只按单 key 编写
// （多 key 由引擎逐 key 执行拆卡）。沙箱无 os/io 库：时间戳只能透传 ISO 字符串
// 或返回 unix 秒数字（契约会把数字归一成字符串，前端再格式化为时间）。
//
// 接口依据摘要：
// - new-api 系中转：GET {base}/api/usage/token/，Bearer sk-key → data.{total_used,
//   total_available, unlimited_quota, expires_at}；quota 默认 500000 = $1（站点可改）
// - DeepSeek：GET api.deepseek.com/user/balance → balance_infos[]（金额为字符串）
// - Moonshot：GET api.moonshot.cn/v1/users/me/balance → data.available/voucher/cash_balance
// - SiliconFlow：GET api.siliconflow.cn/v1/user/info → data.balance/chargeBalance/totalBalance
// - OpenRouter：GET openrouter.ai/api/v1/credits + /api/v1/key → credits 相减得余额
// - 智谱 Coding Plan：GET open.bigmodel.cn/api/monitor/usage/quota/limit（裸 key 鉴权）
//   → data.limits[].{type, percentage, nextResetTime(ms)}
// - Kimi Coding Plan：GET api.kimi.com/coding/v1/usages → credits.monthlyCredits +
//   windowLimits.{fiveHour,weekly}.{used,cap,resetAt(ms)}
// - Claude Code 订阅：GET api.anthropic.com/api/oauth/usage（OAuth token + 必需头）
//   → five_hour/seven_day.{utilization, resets_at(ISO)}

/** 新建卡片时可选的脚本模板。 */
export interface BalanceTemplate {
  id: string;
  label: string;
  /** 模板适用面与注意事项（选中后展示在表单里）。 */
  description: string;
  /** 建议的查询 URL（选模板时填入表单，用户可改）；不填表示脚本内置地址。 */
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
-- new-api 系中转站：GET {base}/api/usage/token/ 查询本 key 额度
-- 适用：new-api 及实现了该端点的兼容站点（one-api / Veloera / done-hub 没有
-- key 维度查询接口，不能用本模板）。查询 URL 填站点根地址（不带 /v1）。

function MB.query(ctx)
  if ctx.base_url == "" then
    return { status = "error", message = "请在卡片上填写查询 URL（站点根地址，不带 /v1）" }
  end
  local r = mb.http.request({
    method = "GET",
    url = ctx.base_url .. "/api/usage/token/",
    headers = { { "authorization", "Bearer " .. ctx.key } },
    timeout_ms = 10000,
  })
  if r.status ~= 200 then
    return { status = "error", message = "站点返回 HTTP " .. tostring(r.status)
      .. "（站点非 new-api 系或版本过旧，不支持 key 维度查询）" }
  end
  local d = (r.body or {}).data
  if not d then
    return { status = "error", message = "响应缺少 data 字段" }
  end
  if d.unlimited_quota then
    return { status = "ok", summary = (d.name or "该 key") .. "：无限额度", quotas = {} }
  end
  local used = d.total_used or 0
  local avail = d.total_available or 0
  local total = used + avail
  if total <= 0 then
    return { status = "ok", summary = (d.name or "该 key") .. "：无额度记录", quotas = {} }
  end
  -- quota 与货币的换算可被站点自定义；默认 500000 quota = 1 美元，
  -- 不准时在卡片「额外参数 JSON」里改 quota_per_unit / unit
  local extra = ctx.extra or {}
  local qpu = extra.quota_per_unit or 500000
  local unit = extra.unit or "$"
  return {
    status = "ok",
    summary = string.format("%s：剩余 %.2f%s", d.name or "key", avail / qpu, unit),
    quotas = {
      {
        label = "额度",
        used_percent = used / total * 100,
        unit = unit,
        used_amount = used / qpu,
        left_amount = avail / qpu,
        reset_at = (d.expires_at and d.expires_at > 0) and d.expires_at or nil,
      },
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
    url = "https://api.deepseek.com/user/balance",
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
  local h = { { "authorization", "Bearer " .. ctx.key } }
  local r = mb.http.request({
    method = "GET",
    url = "https://openrouter.ai/api/v1/credits",
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
    url = "https://openrouter.ai/api/v1/key",
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
-- percentage 是已用百分比；nextResetTime 是毫秒时间戳（契约允许 reset_at 给 unix 秒数字）。

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
-- Kimi Coding Plan：GET {base}/coding/v1/usages
-- credits.monthlyCredits = 月度剩余额度；windowLimits.fiveHour / weekly 是
-- 5 小时与每周窗口的已用量。各档上限因套餐而异，在卡片「额外参数 JSON」里配
-- monthly_cap / five_hour_cap / weekly_cap / unit。

function MB.query(ctx)
  local base = ctx.base_url ~= "" and ctx.base_url or "https://api.kimi.com"
  local r = mb.http.request({
    method = "GET",
    url = base .. "/coding/v1/usages",
    headers = {
      { "authorization", "Bearer " .. ctx.key },
      { "user-agent", "cli" },
    },
    timeout_ms = 10000,
  })
  if r.status ~= 200 then
    return { status = "error", message = "上游返回 HTTP " .. tostring(r.status) }
  end
  local body = r.body or {}
  local extra = ctx.extra or {}
  local unit = extra.unit or "$"
  local monthlyCap = extra.monthly_cap or 70
  local fiveCap = extra.five_hour_cap or 14
  local weeklyCap = extra.weekly_cap or 35
  local quotas = {}
  local credits = body.credits
  if credits and credits.monthlyCredits then
    local left = credits.monthlyCredits
    table.insert(quotas, {
      label = "月度",
      unit = unit,
      used_amount = monthlyCap - left,
      left_amount = left,
    })
  end
  local wl = body.windowLimits or {}
  local function window(label, w, cap)
    if not w or not w.used then return end
    table.insert(quotas, {
      label = label,
      used_percent = w.used / cap * 100,
      reset_at = w.resetAt and math.floor(w.resetAt / 1000) or nil,
    })
  end
  window("5 小时", wl.fiveHour, fiveCap)
  window("Weekly", wl.weekly, weeklyCap)
  if #quotas == 0 then
    return { status = "error", message = "响应缺少 credits / windowLimits（该 key 可能不是 Coding Plan）" }
  end
  local summary = credits and credits.monthlyCredits
    and string.format("月度剩余 %.2f%s / %g%s", credits.monthlyCredits, unit, monthlyCap, unit)
    or nil
  return { status = "ok", summary = summary, quotas = quotas }
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
    url = "https://api.anthropic.com/api/oauth/usage",
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

/** 内置模板清单（新建卡片时的选择顺序即数组顺序）。 */
export const BALANCE_TEMPLATES: BalanceTemplate[] = [
  {
    id: "generic-percent",
    label: "通用百分比",
    description: "最通用的骨架：Bearer 鉴权 GET 一个地址，把返回字段映射成百分比配额。其他服务都不匹配时从它改起。",
    script: GENERIC_PERCENT,
  },
  {
    id: "generic-amount",
    label: "通用金额",
    description: "金额口径骨架：返回「消耗/余额」金额字段，币种走额外参数 unit（默认 ¥）。",
    extraText: '{\n  "unit": "¥"\n}',
    script: GENERIC_AMOUNT,
  },
  {
    id: "new-api",
    label: "new-api 中转站",
    description:
      "适用于 new-api 及兼容站点：GET {URL}/api/usage/token/ 查本 key 额度。查询 URL 填站点根地址（不带 /v1）；one-api/Veloera/done-hub 不支持 key 维度查询。quota 换算默认 500000=$1，可在额外参数改。",
    extraText: '{\n  "quota_per_unit": 500000,\n  "unit": "$"\n}',
    script: NEW_API,
  },
  {
    id: "deepseek",
    label: "DeepSeek",
    description: "DeepSeek 官方余额接口，逐币种账户各出一条配额（金额为字符串，自动转数字）。地址已内置，查询 URL 留空即可。",
    script: DEEPSEEK,
  },
  {
    id: "moonshot",
    label: "Moonshot / Kimi 开放平台",
    description: "开放平台账户余额：可用 = 充值 + 代金券（人民币元）。默认国内站 api.moonshot.cn，国际站把查询 URL 改为 https://api.moonshot.ai。注意与 Kimi Coding Plan 是两个产品、key 不通用。",
    script: MOONSHOT,
  },
  {
    id: "siliconflow",
    label: "SiliconFlow 硅基流动",
    description: "总余额 / 充值 / 赠金三条配额（人民币元）。默认国内站 api.siliconflow.cn，国际站把查询 URL 改为 https://api.siliconflow.com。",
    script: SILICONFLOW,
  },
  {
    id: "openrouter",
    label: "OpenRouter",
    description: "账户余额（累计购入 − 累计已用，美元）；key 设了限额时附带限额百分比条。地址已内置，查询 URL 留空即可。",
    script: OPENROUTER,
  },
  {
    id: "zhipu-glm",
    label: "智谱 GLM Coding Plan",
    description: "GLM Coding Plan 窗口配额（5 小时 / 周窗口的已用百分比 + 重置时间）。默认国内 open.bigmodel.cn，国际站把查询 URL 改为 https://api.z.ai。鉴权自动尝试裸 key 与 Bearer 两种形式。",
    script: ZHIPU_GLM,
  },
  {
    id: "kimi-coding",
    label: "Kimi Coding Plan",
    description: "Kimi 编程订阅：月度剩余额度（金额口径）+ 5 小时 / 每周窗口（已用百分比 + 重置时间）。各档上限因套餐而异，在额外参数里配 monthly_cap / five_hour_cap / weekly_cap / unit。默认地址 https://api.kimi.com。",
    extraText: '{\n  "monthly_cap": 70,\n  "five_hour_cap": 14,\n  "weekly_cap": 35,\n  "unit": "$"\n}',
    script: KIMI_CODING,
  },
  {
    id: "claude-code",
    label: "Claude Code 订阅",
    description: "Claude Code 订阅窗口配额（5 小时 / 7 天已用百分比 + 重置时间）。注意：上游服务里要放 Claude Code 的 OAuth access token（不是 sk-ant API key）；接口有限流，建议间隔 ≥ 300 秒。",
    intervalSecs: 300,
    script: CLAUDE_CODE,
  },
];

/** 新建卡片的默认脚本（通用百分比模板）。 */
export const DEFAULT_BALANCE_SCRIPT = GENERIC_PERCENT;
