MB = { category = "quota" }
-- 智谱 GLM Coding Plan：GET {base}/api/monitor/usage/quota/limit
-- 国内 open.bigmodel.cn / 国际 api.z.ai；鉴权用裸 key（不带 Bearer），失败回退 Bearer。
-- data.limits[] 的 type：TIME_LIMIT = 5 小时窗口，TOKENS_LIMIT = token 周窗口；
-- percentage 是已用百分比；nextResetTime 是毫秒时间戳（reset_at 也可以给 unix 秒数字）。

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
    local label, period = "配额", nil
    if w.type == "TIME_LIMIT" then
      label, period = "5 小时窗口", 5 * 3600
    elseif w.type == "TOKENS_LIMIT" then
      label, period = "周窗口", 7 * 86400
    end
    table.insert(quotas, {
      type = "percentage",
      label = label,
      period_secs = period,
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
