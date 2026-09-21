MB = { category = "quota" }
-- Sub2API：GET {base}/v1/usage；优先 Key 剩余额度，其次钱包余额/套餐剩余额度，
-- 附带 rate_limits 里 5h/1d/7d 周期剩余。查询 URL 填站点根地址或以 /v1 结尾。

function MB.query(ctx)
  local base = (ctx.base_url or ""):match("^%s*(.-)%s*$")
  base = base:gsub("/+$", ""):gsub("/v1$", "")
  if base == "" then
    return { status = "error", message = "请在上游服务端点填写查询 URL（Sub2API 站点地址）" }
  end
  if not ctx.key or ctx.key == "" then
    return { status = "error", message = "请填写 Sub2API API Key" }
  end
  local r = mb.http.request({
    method = "GET",
    url = base .. "/v1/usage",
    headers = { { "authorization", "Bearer " .. ctx.key } },
    timeout_ms = 10000,
  })
  if r.status ~= 200 then
    local message = "查询失败，HTTP " .. tostring(r.status)
    if r.status == 401 then
      message = message .. "：API Key 无效或未被接受"
    elseif r.status == 403 then
      message = message .. "：访问被拒绝，请检查 Key 状态及访问限制"
    elseif r.status == 404 then
      message = message .. "：请检查站点地址及是否支持 /v1/usage"
    end
    return { status = "error", message = message }
  end
  local d = r.body
  if type(d) ~= "table" then
    return { status = "error", message = "接口未返回有效的 JSON 对象" }
  end
  if d.isValid == false then
    return { status = "error", message = "接口返回 Key 不可用" }
  end
  local quotas = {}
  local function add(label, amount, period)
    local value = tonumber(amount)
    if value ~= nil then
      quotas[#quotas + 1] = { type = "quota", label = label, unit = "$", left_amount = value, period_secs = period }
      return true
    end
    return false
  end
  local has_amount = false
  if type(d.quota) == "table" then
    has_amount = add("Key 剩余额度", d.quota.remaining)
  end
  if not has_amount then
    has_amount = add("钱包余额", d.balance)
  end
  if not has_amount then
    local label = "剩余额度"
    if d.mode == "quota_limited" then
      label = "Key 剩余额度"
    elseif d.planName == "钱包余额" then
      label = "钱包余额"
    elseif d.planName then
      label = tostring(d.planName) .. " 剩余额度"
    end
    add(label, d.remaining)
  end
  local windows = { ["5h"] = { "5 小时", 5 * 3600 }, ["1d"] = { "1 天", 86400 }, ["7d"] = { "7 天", 7 * 86400 } }
  if type(d.rate_limits) == "table" then
    for _, limit in ipairs(d.rate_limits) do
      if type(limit) == "table" then
        local window = tostring(limit.window or "")
        local meta = windows[window] or { window, nil }
        add(meta[1] .. "周期剩余额度", limit.remaining, meta[2])
      end
    end
  end
  if #quotas == 0 then
    return {
      status = "error",
      message = "响应未包含余额或剩余额度，可能是订阅信息不可用或站点版本不兼容",
    }
  end
  return { status = "ok", quotas = quotas }
end
