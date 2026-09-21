MB = { category = "quota" }
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
  local function window(label, period, w)
    if not w or not w.utilization then return end
    table.insert(quotas, {
      type = "percentage",
      label = label,
      period_secs = period,
      used_percent = w.utilization,
      reset_at = w.resets_at,
    })
  end
  window("5 小时", 5 * 3600, body.five_hour)
  window("7 天", 7 * 86400, body.seven_day)
  window("7 天（Opus）", 7 * 86400, body.seven_day_opus)
  window("7 天（Sonnet）", 7 * 86400, body.seven_day_sonnet)
  if #quotas == 0 then
    return { status = "error", message = "响应缺少窗口数据" }
  end
  return { status = "ok", quotas = quotas }
end
