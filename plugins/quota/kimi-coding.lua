MB = { category = "quota" }
-- Kimi Coding Plan：GET {base}/coding/v1/usages（user-agent: cli 必需）

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
  for _, item in ipairs({ { "limit_5h", "5 小时", 5 * 3600 }, { "limit_7d", "Weekly", 7 * 86400 } }) do
    local w = usages[item[1]]
    local value = type(w) == "table" and w.used_ratio or nil
    local ratio = (type(value) == "number" or type(value) == "string") and tonumber(value) or nil
    if ratio and ratio == ratio and ratio >= 0 and ratio < math.huge and ratio * 100 < math.huge then
      table.insert(quotas, {
        type = "percentage",
        label = item[2],
        period_secs = item[3],
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
