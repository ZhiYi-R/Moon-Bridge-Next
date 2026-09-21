MB = { category = "quota" }
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
    { type = "quota", label = "账户余额", unit = "$", used_amount = cd.total_usage, left_amount = left },
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
      type = "percentage",
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
