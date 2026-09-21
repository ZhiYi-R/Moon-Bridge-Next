MB = {
  category = "quota",
  config_schema = {
    quota_per_unit = { type = "number", label = "每单位额度", default = 500000, help = "data.quota 除以该值得到余额（new-api 默认 500000 = $1）" },
    unit = { type = "string", label = "货币单位", default = "$" },
  },
}
-- new-api 中转站：GET {base}/api/user/self，data.quota / quota_per_unit（默认 500000）作余额。
-- 查询 URL 填站点根地址（不带 /v1）；请使用能访问账户接口的凭据。

local function number(value)
  if type(value) ~= "number" and type(value) ~= "string" then return nil end
  local n = tonumber(value)
  if not n or n ~= n or n == math.huge or n == -math.huge then return nil end
  return n
end

function MB.query(ctx)
  local base = (ctx.base_url or ""):match("^%s*(.-)%s*$"):gsub("/+$", "")
  if base == "" then
    return { status = "error", message = "请填写查询 URL（站点根地址，不带 /v1）" }
  end
  local extra = type(ctx.extra) == "table" and ctx.extra or {}
  local qpu = number(extra.quota_per_unit == nil and 500000 or extra.quota_per_unit)
  if not qpu or qpu <= 0 then
    return { status = "error", message = "quota_per_unit 必须是大于 0 的有限数字" }
  end
  local r = mb.http.request({
    method = "GET",
    url = base .. "/api/user/self",
    headers = { { "authorization", "Bearer " .. ctx.key } },
    timeout_ms = 10000,
  })
  if r.status ~= 200 then
    return { status = "error", message = "站点返回 HTTP " .. tostring(r.status) }
  end
  local body = type(r.body) == "table" and r.body or {}
  if body.success == false or body.error then
    return { status = "error", message = tostring(body.message or body.error or "账户查询失败") }
  end
  local d = type(body.data) == "table" and body.data or {}
  local quota = number(d.quota)
  if not quota then
    return { status = "error", message = "响应缺少有效的 data.quota" }
  end
  if quota <= 0 then
    return { status = "ok", summary = "无额度记录", quotas = {} }
  end
  return {
    status = "ok",
    quotas = {
      { type = "quota", label = "额度", unit = extra.unit or "$", left_amount = quota / qpu },
    },
  }
end
