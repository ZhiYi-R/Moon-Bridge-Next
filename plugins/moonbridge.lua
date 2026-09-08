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
---@field capabilities string[] 能力："core"|"raw_request"|"raw_response"|"raw_stream"
---@field config_schema table|nil 配置 JSON Schema（供 UI 渲染表单）
---@field entry string|nil 入口提示
---@field init fun()|nil 加载后调用一次
---@field shutdown fun()|nil 卸载时调用
---Core IR 语义层钩子（需声明 "core"）。
---@field on_request (fun(ctx: MbCtx, req: MbCoreRequest): MbCoreRequest|nil)|nil
---@field on_response (fun(ctx: MbCtx, resp: MbCoreResponse): MbCoreResponse|nil)|nil
---@field inject_tools (fun(ctx: MbCtx): MbTool[])|nil
---@field filter_content (fun(ctx: MbCtx, block: table): boolean|table)|nil 返回 true 跳过该块
---@field transform_error (fun(ctx: MbCtx, msg: string): string)|nil
---出入站原始报文钩子（需声明对应 raw_* 能力）。
---@field on_client_request_raw (fun(ctx: MbCtx, msg: MbRawMessage): MbVerdict|nil)|nil
---@field on_upstream_request_raw (fun(ctx: MbCtx, msg: MbRawMessage): MbVerdict|nil)|nil
---@field on_upstream_response_raw (fun(ctx: MbCtx, msg: MbRawMessage): MbVerdict|nil)|nil
---@field on_client_response_raw (fun(ctx: MbCtx, msg: MbRawMessage): MbVerdict|nil)|nil
---流式 chunk 钩子（需声明 "raw_stream"；高频，未声明则完全跳过）。
---@field on_upstream_chunk_raw (fun(ctx: MbCtx, chunk: MbRawChunk): MbChunkVerdict|nil)|nil
---@field on_client_chunk_raw (fun(ctx: MbCtx, chunk: MbRawChunk): MbChunkVerdict|nil)|nil

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
---@field metadata table|nil 请求级元数据（session_id、原始 headers 等）

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
---@field url string 完整 URL（经宿主 egress proxy / 超时 / 域名白名单约束）
---@field headers MbHeaderList|nil
---@field body table|nil JSON 请求体
---@field timeout_ms number|nil

---@class MbHttpResponse
---@field status number
---@field headers MbHeaderList
---@field body any JSON 优先解析，否则为字符串

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
---@field crypto { sha256: fun(s: string): string, hmac_sha256: fun(key: string, msg: string): string, base64_encode: fun(s: string): string, base64_decode: fun(s: string): string }
---header 辅助（大小写不敏感，操作保序数组）。
---@field headers MbHeadersHelper

---@type MbHostApi
mb = {}
