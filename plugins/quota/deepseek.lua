MB = { category = "quota" }
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
      type = "quota",
      label = "余额（" .. cur .. "）",
      unit = unit,
      left_amount = tonumber(info.total_balance),
    })
  end
  return { status = "ok", summary = tostring(#infos) .. " 个币种账户", quotas = quotas }
end
