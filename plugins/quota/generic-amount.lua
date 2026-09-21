MB = {
  category = "quota",
  config_schema = {
    unit = { type = "string", label = "货币单位", default = "¥" },
  },
}
-- 通用金额模板：读取已用/剩余金额；币种从配额配置 unit 取（默认 ¥）

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
      { type = "quota", label = "余额", unit = unit, used_amount = d.used_amount, left_amount = left },
    },
  }
end
