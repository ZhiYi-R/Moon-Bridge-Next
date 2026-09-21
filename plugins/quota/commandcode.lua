MB = {
  category = "quota",
  config_schema = {
    monthly_cap = { type = "number", label = "月总额度", default = 70, help = "credits.monthlyCredits 对应的月度上限" },
    unit = { type = "string", label = "货币单位", default = "" },
  },
}
-- CommandCode：GET {base}/alpha/billing/credits
-- credits.monthlyCredits + windowLimits.{fiveHour,weekly}.{used,cap,resetAt(ms)}；
-- 月总额度从配额配置 monthly_cap 取（默认 70），单位 unit。

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
      type = "quota",
      label = "Monthly",
      period_secs = 30 * 86400,
      unit = unit,
      used_amount = round2(monthly_cap - monthly_left),
      left_amount = round2(math.max(monthly_left, 0)),
    })
  end
  local windows = type(body.windowLimits) == "table" and body.windowLimits or {}
  for _, item in ipairs({ { "fiveHour", "5H", 5 * 3600 }, { "weekly", "Weekly", 7 * 86400 } }) do
    local w = windows[item[1]]
    if w ~= nil then
      local used = type(w) == "table" and tonumber(w.used) or nil
      local cap = type(w) == "table" and tonumber(w.cap) or nil
      if not used or not cap or cap < 0 then
        return { status = "error", message = "响应缺少有效的 windowLimits." .. item[1] .. ".used/cap" }
      end
      local reset = tonumber(w.resetAt)
      table.insert(quotas, {
        type = "quota",
        label = item[2],
        period_secs = item[3],
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
