MB = { category = "quota" }
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
      { type = "quota", label = "总余额", unit = "¥", left_amount = tonumber(d.totalBalance) },
      { type = "quota", label = "充值余额", unit = "¥", left_amount = tonumber(d.chargeBalance) },
      { type = "quota", label = "赠金余额", unit = "¥", left_amount = tonumber(d.balance) },
    },
  }
end
