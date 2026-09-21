MB = { category = "quota" }
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
      { type = "percentage", label = "5 小时窗口", period_secs = 5 * 3600, used_percent = d.used_percent, reset_at = d.reset_at },
      { type = "percentage", label = "周额度", period_secs = 7 * 86400, used_percent = d.weekly_used_percent },
    },
  }
end
