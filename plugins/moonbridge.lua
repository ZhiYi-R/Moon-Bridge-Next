---@meta
-- Moon Bridge Next 插件 API Stub（仅供 lua-language-server / LSP 使用，勿在插件中 require）。
--
-- 用法：在编辑器中把本文件加入 lua-language-server 的工作区库，即可获得 `mb.*` 与
-- `MB` 钩子的补全与类型提示。例如 VS Code（sumneko lua）settings.json：
--
--   "Lua.workspace.library": { "/path/to/moon-bridge-next/plugins": true }
--
-- 字段说明与 crates/plugin/src/{host,runtime,convert,bridge}.rs 保持同步。
--
-- 约定：插件脚本在全局暴露 `MB` table——既承载清单（name/version/scopes/capabilities），
-- 也承载钩子函数；宿主 API 挂在全局 `mb`（小写）。沙箱中 `os/io/loadfile/dofile` 不可用。

--------------------------------------------------------------------------------
-- 清单与钩子（全局 MB 表）
--------------------------------------------------------------------------------

---@class MbVerdict
---报文钩子返回值：nil 表示放行（就地修改经 table 引用回写）；
---`{ action = "short_circuit", status?, headers?, body? }` 短路本次请求/响应；
---`{ action = "abort" }` 中止。
---@field action string|nil "short_circuit"|"abort"|nil
---@field status number|nil short_circuit 时的 HTTP 状态码（默认 200）
---@field headers table[]|nil short_circuit 时的响应头 `{{k,v},...}`
---@field body any short_circuit 时的响应体（table/字符串）

---@class MbChunkVerdict
---chunk 钩子返回值：nil 放行；`{ action = "drop" }` 丢弃该 chunk。
---@field action string|nil "drop"|nil

---@class MbManifest
---@field name string 唯一插件名
---@field version string|nil 版本
---@field scopes string[]|nil 作用域："global"|"provider"|"model"|"route"
---@field capabilities string[] 能力："core"|"raw_request"|"raw_response"|"raw_stream"|"auth"
---@field requires table|nil 启用门控：`{ <网关配置键> = <期望值>, ... }`（bool/int/float/字符串）。
---仅 app 层校验——启用动作（新建即启用 / 停用→启用）时不满足则拒绝并弹提示；网关侧不感知。
---@field config_schema table|nil 配置 JSON Schema（供 UI 渲染表单）
---@field entry string|nil 入口提示
---@field fs_read_allow string[]|nil `mb.fs.read` 可读路径白名单（home 相对 `~/...`，精确匹配，不支持通配；未声明即完全禁止读文件）
---@field init fun()|nil 加载后调用一次
---@field shutdown fun()|nil 卸载时调用
---Core IR 语义层钩子（需声明 "core"）。
---@field on_request (fun(ctx: MbCtx, req: MbCoreRequest): MbCoreRequest|nil)|nil
---@field on_response (fun(ctx: MbCtx, resp: MbCoreResponse): MbCoreResponse|nil)|nil
---@field inject_tools (fun(ctx: MbCtx): MbTool[])|nil
---@field filter_content (fun(ctx: MbCtx, block: table): boolean|table)|nil 返回 true 跳过该块
---@field on_stream_event (fun(ctx: MbCtx, ev: table): boolean)|nil 处理单个流事件；返回 true 丢弃该事件
---@field transform_error (fun(ctx: MbCtx, msg: string): string)|nil
---出入站原始报文钩子（需声明对应 raw_* 能力）。
---@field on_client_request_raw (fun(ctx: MbCtx, msg: MbRawMessage): MbVerdict|nil)|nil
---@field on_upstream_request_raw (fun(ctx: MbCtx, msg: MbRawMessage): MbVerdict|nil)|nil
---@field on_upstream_response_raw (fun(ctx: MbCtx, msg: MbRawMessage): MbVerdict|nil)|nil
---@field on_client_response_raw (fun(ctx: MbCtx, msg: MbRawMessage): MbVerdict|nil)|nil
---流式 chunk 钩子（需声明 "raw_stream"；高频，未声明则完全跳过）。
---@field on_upstream_chunk_raw (fun(ctx: MbCtx, chunk: MbRawChunk): MbChunkVerdict|nil)|nil
---@field on_client_chunk_raw (fun(ctx: MbCtx, chunk: MbRawChunk): MbChunkVerdict|nil)|nil
---认证钩子（需声明 "auth"；provider 作用域绑定生效，由宿主按 provider 解析后显式调用，不进入报文链路）。
---@field auth_describe (fun(ctx: MbAuthCtx): MbAuthDescribe)|nil 流程元数据（UI 据此渲染）
---@field auth_begin (fun(ctx: MbAuthCtx): MbAuthBeginResult)|nil 发起登录（可直出 done/error 终态）
---@field auth_poll (fun(ctx: MbAuthCtx, handle: table, paste: string|nil): MbAuthPollResult)|nil 单步推进登录（编排器掌握循环/超时/取消）
---@field auth_refresh (fun(ctx: MbAuthCtx, bundle: MbAuthBundle): MbAuthBundle)|nil 平台刷新授予
---@field auth_headers (fun(ctx: MbAuthCtx, bundle: MbAuthBundle): MbHeaderList)|nil 产出认证头（Authorization + 平台专有头）
---余额&健康看板：**独立于插件**的一次性脚本入口——由「余额看板」卡片引用并调用，
---不进插件注册表、不参与上面的能力门控与钩子链路（见 crates/gateway/src/balance.rs）。
---@field query (fun(ctx: MbBalanceQueryCtx): MbBalanceReturn)|nil 查询一次余额/配额

--------------------------------------------------------------------------------
-- 余额&健康看板（MB.query）
--------------------------------------------------------------------------------

---@class MbBalanceQuota
---@field label string 配额展示名（如「5 小时窗口」）
---@field used_percent number|nil 已用百分比；与 left_percent 互补，只给一个即可
---@field left_percent number|nil 剩余百分比
---@field unit string|nil 金额/数量单位（如 "¥"、"GB"），金额模式使用
---@field used_amount number|nil 已用金额/数量（金额模式；amount 与 percent 互不推导）
---@field left_amount number|nil 剩余金额/数量（金额模式）
---@field reset_at string|integer|nil 重置时间：字符串原样展示；或给 unix 秒数字（引擎归一为字符串，前端按本地时间格式化——沙箱无 os 库，毫秒时间戳请除 1000 取整后给出）

---@class MbBalanceReturn
---返回 table，经宿主序列化为 JSON 落库。
---@field status string|nil "ok"|"error"（缺省 = ok）
---@field message string|nil 失败原因（status = "error" 时展示给用户）
---@field quotas MbBalanceQuota[]|nil 配额列表
---@field summary string|nil 一句话摘要
---额外字段原样保留在结果 payload 中。

---@class MbBalanceQueryCtx
---@field name string 卡片 key
---@field key string 本次查询的 API Key（与 keys[1] 相同）
---@field keys string[] 当前 key 的单元素数组——引擎对卡片解析出的每个 key 各调用一次 MB.query 并拆成多张卡片展示，脚本只需按单 key 编写（引用上游服务时 key 由其端点解析并去重；端点 key 留空回退前一个非空 key）
---@field base_url string 卡片上可选填写的查询 URL（配额接口基准地址；与 Provider 端点无关，可能为空字符串）
---@field provider string 服务商标识（Provider key）
---@field extra table 卡片自定义参数（编辑表单「额外参数 JSON」的解码值）

---@type MbManifest
MB = {}

--------------------------------------------------------------------------------
-- 上下文（每次钩子调用注入的只读 table）
--------------------------------------------------------------------------------

---@class MbCtx
---@field request_id string 请求 ID
---@field session_id string|nil 会话 ID（无会话时为 nil）
---@field model_alias string 路由前模型别名
---@field client_protocol string 入口协议："openai-response"|"openai-chat"|"anthropic"|"google-genai"
---@field upstream_protocol string|nil 上游协议（未路由时为 nil）
---@field provider string|nil 上游服务 key（未路由时为 nil）
---@field stream boolean 是否流式请求

--------------------------------------------------------------------------------
-- Core IR（on_request / on_response 的操作对象；就地修改即生效）
--------------------------------------------------------------------------------

---@class MbTool
---@field name string
---@field description string|nil
---@field input_schema table|nil JSON Schema

---@class MbCoreRequest
---@field model string 路由解析后的上游模型名
---@field model_alias string 客户端请求的模型别名（路由前）
---@field system table system 指令（ContentBlock 数组，独立于 messages）
---@field messages table 消息数组
---@field tools MbTool[]
---@field tool_choice table|nil
---@field max_tokens number|nil
---@field temperature number|nil
---@field top_p number|nil
---@field stop string[]
---@field stream boolean
---@field reasoning table|nil 推理配置
---@field meta table|nil 请求级元数据（session_id、原始 headers、客户端标识等）

---@class MbCoreResponse
---@field content table ContentBlock 数组
---@field stop_reason string|nil "end_turn"|"max_tokens"|...
---@field usage table|nil { input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, reasoning_tokens }

--------------------------------------------------------------------------------
-- 原始报文（raw_* 钩子；body 为 JSON table / 字符串 / nil）
--------------------------------------------------------------------------------

---@class MbRawMessage
---@field stage string "client_request"|"upstream_request"|"upstream_response"|"client_response"
---@field protocol string
---@field provider string|nil
---@field method string
---@field url string
---@field status number
---@field headers table[] 请求/响应头，保序数组 `{{k,v},...}`（配 mb.headers 操作）
---@field body any JSON table / 文本字符串 / nil；超上限时为 nil 且 body_truncated=true
---@field body_truncated boolean|nil

---@class MbRawChunk
---@field stage string "upstream_chunk"|"client_chunk"
---@field protocol string
---@field provider string|nil
---@field event string SSE event 名
---@field data any JSON table / 字符串 / nil；超上限时为 nil 且 data_truncated=true
---@field data_truncated boolean|nil
---@field raw string 原始报文文本

--------------------------------------------------------------------------------
-- 宿主 API（全局 mb 表；网络与跨 provider 调用为异步函数，可直接 await 语义使用）
--------------------------------------------------------------------------------

---@class MbHeaderList
---保序 header 数组，形如 `{{"Content-Type","application/json"}}`；元素为 {string, string}。

---@class MbHeadersHelper
---@field get fun(hs: MbHeaderList, name: string): string|nil 大小写不敏感读取
---@field set fun(hs: MbHeaderList, name: string, val: string): MbHeaderList 存在则改，否则追加
---@field remove fun(hs: MbHeaderList, name: string): MbHeaderList 移除同名项（全部）

---@class MbHttpRequest
---@field method string|nil 默认 "GET"
---@field url string 完整 URL（经宿主 egress proxy 与兜底超时；**无目标白名单/过滤**——收口≠授权）
---@field headers MbHeaderList|nil
---@field body table|nil JSON 请求体
---@field timeout_ms number|nil

---@class MbHttpResponse
---@field status number
---@field headers MbHeaderList
---@field body any JSON 优先解析，否则为字符串

---@class MbAuthCtx
---认证钩子上下文（非报文钩子 ctx；由宿主按 provider 构造）。
---@field provider string 上游服务 key（= 账户预设 id）
---@field source string|nil 登录来源（用户在 UI 的选择；插件在 describe.sources 中广告可选值）
---@field host { os: string, arch: string, name: string }|nil 宿主平台信息（沙箱无 os 库；设备指纹类请求头用）

---@class MbAuthBundle
---令牌包：插件自有 JSON。core 只认两个约定字段，其余字段（refresh/device_id/…）随包透传。
---@field access string 出站凭据（非空，core 校验）
---@field expires_at number|nil 过期时刻（unix 毫秒，**服务端原始值**；缺省=永不过期。skew 由 core 统一扣，插件不得预先扣减）

---@class MbAuthDescribe
---@field kind string "device_code"|"callback"|"paste"（本轮 UI 支持 device_code/callback）
---@field label string|nil 展示名
---@field instructions string|nil 指引文案
---@field supports_paste boolean|nil 是否展示手动粘贴输入框
---@field sources { id: string, label: string }[]|nil 可选凭据来源（多个时 UI 先让用户选，经 ctx.source 回传）

---@class MbAuthBeginResult
---@field status string|nil "done"（本地导入命中，bundle 直出）/"error"（显式失败，message 说明）；缺省=进入轮询流
---@field message string|nil error 时的原因
---@field bundle MbAuthBundle|nil done 时的令牌包
---@field verification_url string|nil 需要用户打开的页面
---@field user_code string|nil 设备码（device_code 流）
---@field interval_secs number|nil 建议轮询间隔（秒，默认 2，限 1..30）
---@field expires_in_secs number|nil 流程总超时（秒，默认 120，限 30..1800）
---@field notice string|nil 提示（如「未检测到本地凭据，已改用浏览器登录」）
---@field handle table|nil 轮询句柄（插件私有，auth_poll 原样收回）

---@class MbAuthPollResult
---@field status string "pending"|"slow_down"|"done"|"error"|"expired"
---@field message string|nil pending 时的进度提示 / error|expired 时的原因
---@field bundle MbAuthBundle|nil done 时的令牌包
---@field interval_secs number|nil slow_down 时的新间隔（秒，限 1..60）

---@class MbCallbackSpec
---@field path string|nil 监听路径（默认 "/callback"；仅 / 开头的安全字符）
---@field origins string[]|nil CORS 允许源（浏览器页面内 fetch 回调时按源钉死；空数组不发 CORS 头）
---@field preferred_port number|nil 首选端口（被占用时宿主退随机端口）

---@class MbCallbackHandle
---@field id string 监听 id
---@field port number 实际绑定端口
---@field url string 完整回调 URL（http://127.0.0.1:{port}{path}）

---@class MbHostApi
---日志（target 为 "plugin"，带插件名）。
---@field log { debug: fun(msg: string), info: fun(msg: string), warn: fun(msg: string), error: fun(msg: string) }
---插件配置：来自 store 中该插件的 config JSON。
---@field config table
---跨请求会话状态（按当前会话定位；key/value 均可序列化）。
---@field session { get: fun(key: string): any, set: fun(key: string, val: any) }
---受控 HTTP 子请求（async）。
---@field http { request: async fun(req: MbHttpRequest): MbHttpResponse }
---跨 provider 编排调用：以 Core IR 直接请求另一 provider（async）。
---@field provider { invoke: async fun(providerKey: string, model: string, req: MbCoreRequest): MbCoreResponse }
---摘要与编码工具。
---@field crypto { sha256: fun(s: string): string, hmac_sha256: fun(key: string, msg: string): string, base64_encode: fun(s: string): string, base64_decode: fun(s: string): string, base64url_encode: fun(s: string): string, base64url_decode: fun(s: string): string }
---加密小值（scope 强制按插件名隔离，插件间互不可见；OAuth 令牌包不经过这里——它由宿主在登录编排与出站链路之间直传）。
---@field secret { get: async fun(scope: string, key: string): string|nil, set: async fun(scope: string, key: string, val: string), delete: async fun(scope: string, key: string) }
---CSPRNG（沙箱无安全随机源；OAuth state 等必须从这里取）。
---@field random { state: fun(): string, bytes: fun(n: number): string }
---OAuth 回环回调监听（宿主托管：一次性、带超时、完成即清理；插件不能自己 bind 端口）。
---@field oauth { listen_callback: async fun(spec: MbCallbackSpec): MbCallbackHandle, callback_await: async fun(id: string, timeout_ms: number|nil): table|nil, callback_close: async fun(id: string) }
---在外部浏览器打开 URL（仅 http/https）。
---@field open_external async fun(url: string)
---受限文件读（白名单制：MB.fs_read_allow 声明的 home 相对路径，精确匹配；上限 256KiB）。
---@field fs { read: async fun(path: string): string }
---墙钟（沙箱无 os 库；过期时刻换算用）。
---@field time { now_ms: fun(): number }

---header 辅助（大小写不敏感，操作保序数组）。
---@field headers MbHeadersHelper

---@type MbHostApi
mb = {}
