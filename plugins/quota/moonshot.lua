MB = { category = "quota" }
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
      { type = "quota", label = "可用余额", unit = "¥", left_amount = d.available_balance },
      { type = "quota", label = "充值余额", unit = "¥", left_amount = d.cash_balance },
      { type = "quota", label = "代金券", unit = "¥", left_amount = d.voucher_balance },
    },
  }
end
