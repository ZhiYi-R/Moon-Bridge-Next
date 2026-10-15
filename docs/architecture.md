# Moon Bridge Next 架构文档

> 本地 LLM 网关：多协议转换 · Core IR 中间表示 · 双层 Lua 插件 · 用量统计与配额监控。
> 技术栈：Rust (edition 2021) + Tauri 2 + Vue 3 + Vite + TypeScript。

本文档描述当前系统的分层结构、核心类型契约与运行机制。

---

## 1. 设计原则

- **Core IR 居中转换**：所有入口协议先转换为统一的 `CoreRequest / CoreResponse / CoreStreamEvent`，再转换为目标上游协议，将 N×M 的协议适配收敛为 N+M。
- **Workspace 单向分层**：按职责拆分 crate，编译期保证单向依赖，避免循环耦合。
- **钩子接口解耦**：`protocol` 定义 `PluginHooks` trait，`plugin` 基于 `mlua` 实现该 trait，由 `gateway` 在启动时装配注入，`protocol` 不依赖 `plugin`。
- **双层插件钩子**：插件既可在协议归一化后操作 Core IR 语义层（`core`），也可在协议转换外层直接读写原始 HTTP 报文与 SSE chunk（`raw_*`），支持改写、短路、重试、中止与丢弃。

---

## 2. 分层架构与依赖关系

```
crates/core       moonbridge-core      基础层：Core IR + Protocol + CoreError + ModelRef（无内部依赖）
crates/protocol   moonbridge-protocol  Adapter traits + PluginHooks + Raw 载体 + Registry + 四协议实现
crates/plugin     moonbridge-plugin    mlua 运行时 + mb.* 宿主 API + LuaPluginRegistry (impl PluginHooks)
crates/store      moonbridge-store     rusqlite + 版本化迁移 + DAO (provider/model/route/plugin/usage/quota/settings)
crates/gateway    moonbridge-gateway   axum 服务 + 路由解析 + dispatch 编排 + 配额引擎 + usage/trace
crates/server     moonbridge-server    无头服务端：单端口托管 LLM 网关 + /api/* 管理 REST + SPA 静态资源
src-tauri         moonbridge-app       Tauri 桌面壳：IPC commands + 系统托盘 + 引导配置
ui                —                    Vue 3 前端（支持 Tauri IPC / 浏览器 REST / 内存 Mock）
```

**Crate 依赖方向**：

```
core                       (无内部依赖)
protocol  → core
plugin    → core, protocol
store     → core
gateway   → core, protocol, plugin, store
server    → gateway, store, core
app       → gateway, store, core
```

---

## 3. Core IR（`crates/core/src/ir.rs`）

Core IR 使用 Rust enum 与 `serde` 建模，通过 `mlua` 的 `serialize` 特性与 Lua table 自动互转。

```rust
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text { text },
    Image { image_data, media_type },
    Document { source, media_type, data, name },
    ToolUse { id, item_id, name, namespace, input, signature },
    ToolResult { tool_use_id, content, is_error },
    Reasoning { text, signature, redacted },
}
#[serde(rename_all = "lowercase")]
pub enum Role { System, User, Assistant, Tool }
pub struct Message { role, content: Vec<ContentBlock>, ext: Map }
pub struct Tool { name, description, input_schema: Value, ext: Map }

pub struct CoreRequest {
    model, model_alias, system: Vec<ContentBlock>, messages: Vec<Message>, tools: Vec<Tool>,
    tool_choice, max_tokens, temperature, top_p, stop, stream, reasoning, meta: Map,
}
pub struct CoreResponse { id, model, content, stop_reason, usage: Usage, ext }
pub struct Usage { input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, reasoning_tokens }

#[serde(tag = "type", rename_all = "snake_case")]
pub enum CoreStreamEvent {
    MessageStart { id, model },
    BlockStart { index, block },
    BlockDelta { index, delta },
    BlockStop { index, block },
    MessageDelta { stop_reason, usage },
    MessageStop,
    Error { message },
}
```

- **Token 统计口径不变量**：
  - `input_tokens` 为输入总量，包含 `cache_read_tokens` 与 `cache_write_tokens`（Anthropic 入站时将原生 `input_tokens` 与缓存 token 合并）。
  - `output_tokens` 为输出总量，包含 `reasoning_tokens`（Gemini 入站时将 `thoughtsTokenCount` 并入；对部分返回 `completion_tokens < reasoning_tokens` 的非标 OpenAI Chat 兼容上游，适配器自动按 `completion + reasoning` 补足）。
  - `cache_*` 与 `reasoning_tokens` 是总量的子集拆分，用于按价目分项计费，不重复累加。
- **命名约定**：Core IR 内部字段统一使用 `snake_case`（与 Lua 脚本读写保持一致）；面向前端的 DTO（Store 模型、网关状态、应用配置、Trace 统计等）统一使用 `camelCase`。

---

## 4. 协议适配层（`crates/protocol`）

协议适配分为四个象限（入口 Client / 上游 Provider × 非流式 / 流式），均为 `async_trait + Send + Sync`：

| Trait | 职责 |
|-------|------|
| `ClientAdapter` | 入口协议 ↔ Core（非流式）：`to_core_request` / `from_core_response` |
| `ClientStreamAdapter` | Core 流事件 → 客户端 SSE chunk：`encode(ev) -> Vec<RawChunk>` |
| `ProviderAdapter` | Core ↔ 上游协议（非流式）：`from_core_request -> UpstreamRequest` / `to_core_response` |
| `ProviderStreamAdapter` | 上游 SSE chunk → Core 流事件：`decode(chunk) -> Vec<CoreStreamEvent>` |

`builtin_registry()` 为全部四种协议注册四象限适配器，构成 4×4 转换矩阵：

- `adapters/openai_responses/`：OpenAI Responses（`/v1/responses`）
- `adapters/anthropic/`：Anthropic Messages（`/v1/messages`，含 system/tools/tool_choice 映射与连续同角色消息合并）
- `adapters/openai_chat/`：OpenAI Chat Completions（`/v1/chat/completions`）
- `adapters/google_genai/`：Google Generative AI / Gemini（`contents`/`parts`/`systemInstruction`/`functionCall`/`generationConfig` 映射。Gemini 入口未挂载 HTTP 路由，用于矩阵测试与内部调用）

### 跨协议推理凭据（Signature / Encrypted CoT）

多轮思考模式下，上游要求原样回传推理签名或加密凭据。网关在 `adapters/mod.rs` 中通过来源前缀（`ant:` / `oai:` / `gem:` / `chat:`）标记凭据归属：同源协议还原原始凭据，异源协议作为不透明载荷透传，避免向目标上游发送非本协议的伪造签名。

- **OpenAI Responses**：出站自动附加 `include: ["reasoning.encrypted_content"]`；回传上游时仅发送 `encrypted_content`，跳过仅用于展示的明文 `summary`。
- **Anthropic**：加密推理使用 `redacted_thinking` 块（凭据存放在 `data`）；流式编码器对推理块采用惰性开块，避免产生空 `thinking` 块。
- **Gemini**：映射到 thought part 的 `thoughtSignature`（及 functionCall part 的签名）。
- **OpenAI Chat**：解码时以推理明文生成 `chat:` 前缀凭据供客户端回传；异源凭据以 `<mb-cot>…</mb-cot>` 附在 `reasoning_content` 尾部。出站时对历史 assistant 消息同时写入 `reasoning_content` 与 `reasoning`，并合并相邻 assistant 消息，确保思考内容与 `tool_calls` 位于同一消息内。

---

## 5. 双层钩子模型（`PluginHooks`）

### 5.1 Core IR 语义层（capability `core`）

| 钩子 | 语义 |
|------|------|
| `on_request(ctx, &mut req)` | 修改 `CoreRequest` |
| `inject_tools(ctx) -> Vec<Tool>` | 追加工具定义 |
| `on_response(ctx, &mut resp)` | 修改非流式 `CoreResponse` |
| `on_stream_event(ctx, &mut ev) -> bool` | 处理流式事件，返回 `true` 丢弃该事件 |
| `filter_content(ctx, &mut block) -> bool` | 返回 `true` 移除该内容块。非流式在 `on_response` 后逐块过滤；流式在 `BlockStart` 判定，被移除块的 `index` 后续 `BlockDelta` / `BlockStop` 同步压制 |
| `transform_error(ctx, msg) -> String` | 改写返回给客户端的错误消息 |

### 5.2 生命周期层

- `MB.init` / `MB.shutdown` 由 `PluginHooks::init_all` / `shutdown_all` 调用，绑定到网关服务生命周期：端口监听成功后按加载顺序执行 `init`，排空停止后按逆序执行 `shutdown`。
- 单个插件 `init` / `shutdown` 报错仅记录 `warn` 日志，不阻断其他插件。
- `forget_session(session_id)` 在会话 LRU 表淘汰条目时触发，清理 `SessionStore` 中对应会话的插件状态。

### 5.3 原始报文层（capability `raw_request` / `raw_response` / `raw_stream`）

| 钩子 | Capability | 触发阶段 |
|------|-----------|------|
| `on_client_request_raw` | `raw_request` | 入站请求（客户端 → 网关） |
| `on_upstream_request_raw` | `raw_request` | 出站请求（网关 → 上游，每端点尝试触发一次） |
| `on_upstream_response_raw` | `raw_response` | 上游响应（含非 2xx 错误响应及流式零产出失败，此时 `msg.error = true`） |
| `on_client_response_raw` | `raw_response` | 客户端响应（网关 → 客户端，非流式） |
| `on_upstream_chunk_raw` | `raw_stream` | 上游 SSE chunk |
| `on_client_chunk_raw` | `raw_stream` | 客户端 SSE chunk |

报文载体定义（`crates/protocol/src/raw.rs`）：

```rust
pub struct RawMessage {
    pub stage: RawStage,
    pub protocol: Protocol,
    pub provider: Option<String>,
    pub method: Option<String>,
    pub url: Option<String>,
    pub status: Option<u16>,
    pub headers: Vec<(String, String)>,
    pub body: RawBody,
    pub error: bool,
    pub session_id: Option<String>,
}
pub struct RawChunk   { pub stage: RawStage, pub protocol: Protocol, pub provider: Option<String>, pub event: Option<String>, pub data: RawBody, pub raw: String }
pub enum RawBody      { Json { value: Value }, Text { text: String }, Binary { data: Vec<u8> }, Empty }
pub enum RawVerdict   { Pass, ShortCircuit { status: u16, headers: Vec<(String, String)>, body: RawBody }, Retry { delay_ms: u64 }, Abort { message: String } }
pub enum ChunkVerdict { Forward, Drop }
```

**错误路径与 `Retry` 机制**：
- 当上游返回非 2xx 状态码，或流式请求在产生任何内容块前报错/异常中断时，网关构造 `error = true` 的 `RawMessage` 调用 `on_upstream_response_raw`。
- 插件返回 `Retry { delay_ms }` 时，网关等待 `min(delay_ms, plugin_retry_delay_cap_ms)` 后重新执行端点故障转移链。`Retry` 在错误路径上为终局判定，同阶段后续插件不再执行；在其他阶段或成功响应上返回 `Retry` 视为 `Pass`。
- 重试受宿主配置限制：最多重试 `plugin_retry_max` 轮（默认 8，`0` 表示禁用），单次最大延迟 `plugin_retry_delay_cap_ms`（默认 30000ms）。非流式请求的重试共享 `request_timeout_secs` 总超时预算，剩余时间不足以容纳延迟时直接放弃重试并返回原始上游错误。
- 重试次数记录在 Trace 的 `retries` 字段中（为 0 时省略）。错误路径上对 `msg.body` 的修改不会回传给客户端；改写客户端可见错误消息应使用 `transform_error`。

---

## 6. Lua 插件系统（`crates/plugin`）

### 6.1 运行时与沙箱

- **独立虚拟机**：每个插件实例独占一个 `mlua::Lua`（Lua 5.4），通过 `Arc<tokio::sync::Mutex<Lua>>` 串行化访问。
- **全局变量裁剪**：沙箱移除 `os`、`io`、`loadfile`、`dofile`、`require`、`package` 六个标准库入口。
- **执行配额**：
  - **指令计数与超时**：通过 Lua debug hook（默认每 2000 条指令检查一次）统计单次调用指令数（默认上限 2 亿条）与 wall-clock 超时。宿主通过包装 `coroutine.create` / `coroutine.wrap` 将同一预算钩子挂载到插件新建的所有协程上，防止协程逃逸死循环。
  - **内存上限**：通过 `Lua::set_memory_limit` 限制单插件内存（默认 1024 MB）。
  - **大报文降级**：超过 `max_body_bytes`（默认 100 MB）的报文体不反序列化为 Lua table，置为 `nil` 并标记 `body_truncated = true`，写回时保留原始报文。
- **启用条件（`MB.requires`）**：插件可声明对网关配置的依赖（如 `requires = { sessionMarker = true }`）。应用层在启用插件时校验当前配置，不满足时拒绝启用并返回结构化提示。
- **脚本路径解析（`parse_script_ref`）**：以 `.lua` 结尾的 `script_ref` 视为插件目录（`plugins_dir`）下的文件路径，经过规范化与路径穿越检查，越界直接拒绝；其余字符串视为内联 Lua 源码。

### 6.2 宿主 API（`mb.*`）

| API | 说明 |
|-----|------|
| `mb.log.{debug,info,warn,error}(msg)` | 结构化日志（`tracing` target 为 `plugin`） |
| `mb.config` | 当前绑定下的插件配置表 |
| `mb.session.get(k) / set(k, v)` | 按「插件名 + session_id」隔离的内存键值存储 |
| `mb.http.request{method,url,headers,body,timeout_ms}` | 异步 HTTP 请求，经 `HostBridge` 复用宿主客户端（受出站代理与默认 30s 超时约束） |
| `mb.provider.invoke(provider, model, core_request)` | 异步跨 Provider 调用 |
| `mb.headers.{get,set,remove}(headers, name)` | 大小写不敏感、保序的报文头数组辅助函数 |
| `mb.crypto.{sha256,hmac_sha256,base64_encode,base64_decode}` | 哈希与 Base64 工具 |

---

## 7. 存储层（`crates/store`）

使用 SQLite（`rusqlite` bundled，开启 WAL 模式），通过单连接配合 `parking_lot::Mutex` 串行化访问。当前 Schema 版本为 **V14**（定义于 `schema.rs` 的 `MIGRATIONS`）。

| 表名 | 关键列与说明 |
|----|--------|
| `providers` | `key` (PK), `version`, `user_agent`, `web_search_json`, `extra_json`, `enabled`, `quota_plugin_ref`, `quota_interval_secs`, `quota_enabled`, `quota_config_enc`, 时间戳 |
| `provider_endpoints` | `(provider_key, idx)` (PK), `protocol`, `base_url`, `api_key_enc`。单个 Provider 支持多端点，按 `idx` 升序故障转移 |
| `models` | `slug` (PK), `display_name`, `context_window`, `max_output_tokens`, `modalities_json`, `reasoning_levels_json`, `extra_json` |
| `offers` | `(provider_key, model_slug)` (PK), `pricing_json`, `endpoint_protocol`（非空时仅路由至该协议端点） |
| `routes` | `alias` (PK), `model_slug`, `provider_key`, `extra_json` |
| `plugins` | `name` (PK), `source`, `script_ref`, `enabled`, `config_json`, `scopes_json`, `capabilities_json`, `category`（`core` 或 `quota`） |
| `plugin_bindings` | `(plugin_name, scope, scope_key)` (PK), `enabled`, `config_json` |
| `usage_records` | `id` (PK), `session_id`, `model`, `upstream_model`, `provider_key`, 五类 token 计数, `cost`, `status`, `error`, `latency_ms`, `ttft_ms`, `created_at` |
| `quota_results` | `(provider_key, key_index)` (PK), `key_label`（掩码标签）, `status`, `payload_json`, `error`, `queried_at` |
| `settings` | `key` (PK), `value_json` |

- **凭据静态加密**：`provider_endpoints.api_key_enc` 与 `providers.quota_config_enc` 统一使用 AES-GCM 加密。密钥文件默认位于 `<db>.key`（Unix 权限 `0600`，Windows 使用当前用户 DPAPI 封装）。缺失或错误密钥时拒绝打开数据库。备份与跨版本回滚时必须同时保留数据库与密钥文件。
- **费用计算**：`usage::record` 在写入时根据 `(provider_key, upstream_model)` 匹配 Offer 的 `pricing_json` 计算费用（单位 USD / 1M tokens，支持 `tiers` 或 `context_over_200k` 长上下文分档单价）。历史账单保存计算当时的金额，不受后续改价影响。
- **Trace 存储与脱敏**：Trace 报文存放在文件系统 `<data_dir>/traces/<session>/<model>/<created_at>-<short_id>.json`。构造快照时自动将含 `auth`/`api-key`/`apikey`/`token`/`cookie`/`secret` 的请求头及含 `key`/`token`/`secret` 的 URL 查询参数脱敏为 `[REDACTED]`。`trace_record_bodies = false` 时仅保留元数据并清空四个阶段的报文体；`trace_retention`（默认 500）按修改时间全局清理超量文件。

---

## 8. 网关请求生命周期（`crates/gateway`）

```
Client → axum: POST /v1/responses | /v1/messages | /v1/chat/completions
  → Bearer 鉴权 + 初步提取会话标识 (session_id / previous_response_id / X-Codex-Window-Id)
  → [RAW] on_client_request_raw（可修改 headers/body/session_id，或 short_circuit / abort）
  → ClientAdapter::to_core_request → CoreRequest
  → 会话水印解析与剥除：移除历史消息中的所有 [mb:...] 标记，确定最终 ctx.session_id
  → 路由解析：alias → (provider, upstream_model, endpoints)
  → [CORE] on_request + inject_tools
  → 端点故障转移循环（外层为插件 retry 轮次 0..=plugin_retry_max）：
      → ProviderAdapter::from_core_request → UpstreamRequest
      → [RAW] on_upstream_request_raw（回写 method/url/headers/body，可 short_circuit / abort）
      → reqwest 发送请求（遵循 egress_proxy 与非流式总超时预算）
      → 若连接失败或返回 429/5xx 且存在下一端点：切换下一端点重试
  → 若上游返回非 2xx：
      → [RAW] on_upstream_response_raw (msg.error = true)
      → Retry：等待延迟后进入下一轮插件重试；ShortCircuit / Abort：按指令结束；
      → Pass：调用 [CORE] transform_error 并返回上游错误
  → [流式响应]：
      → 逐帧执行 [RAW] on_upstream_chunk_raw → ProviderStreamAdapter::decode
      → 若流在产出任何内容块前失败/中断：触发 on_upstream_response_raw (msg.error = true) 走重试判定
      → [CORE] on_stream_event → [CORE] filter_content → 注入会话水印（首个推理块首部）
      → ClientStreamAdapter::encode → [RAW] on_client_chunk_raw → 写入客户端 SSE
  → [非流式响应]：
      → [RAW] on_upstream_response_raw → ProviderAdapter::to_core_response
      → [CORE] on_response → [CORE] filter_content → 注入会话水印（首个推理块首部）
      → ClientAdapter::from_core_response → [RAW] on_client_response_raw → 写入响应
  → 审计收尾：写入 usage_records 与磁盘 Trace 文件
```

关键实现细节：

- **会话水印（`session.rs`）**：针对不发送会话 ID 的客户端，网关在响应的首个非加密 `Reasoning` 块明文开头插入 `" [mb:<载荷>]"`。网关自建会话的载荷即为会话 UUID；外部长会话 ID 则映射为 6 位十六进制短标签并保存在 LRU `SessionTable` 中（淘汰时联动清理 `SessionStore`）。下一轮请求入站后，`extract_from_request` 在路由与插件执行前剥除全部水印，确保上游模型不会看到或模仿该标记。
- **审计守卫（`InflightAudit` 与 `StreamAudit::Drop`）**：非流式请求在正常返回前若连接断开（客户端取消或任务中止），由 `InflightAudit::Drop` 补写一条 `status = "aborted"` 的用量与 Trace 记录；流式请求进入 SSE 生成器后交由 `StreamAudit::Drop` 负责收尾，并将 `StreamAssembler` 聚合出的完整响应体写入 Trace。所有 `ShortCircuit` 与 `Abort` 提前返回路径均经过 `finish_audit` 落盘审计。

---

## 9. Tauri 桌面层（`src-tauri`）

- **引导配置**：`<config_dir>/config.toml` 存储 `AppConfig { gateway: GatewayConfig, logLevel, autoStart }`，其余业务数据存储在 SQLite。
- **状态管理**：`ManagedState { db, paths, config, gateway }`。网关启停通过 `has_live_gateway()` 短临界区检查避免并发重入死锁。
- **IPC Commands**：提供网关控制（`gateway_start/stop/restart/status`）、实体 CRUD（`provider_*` / `model_*` / `offer_*` / `route_*` / `plugin_*` / `binding_*`）、模型目录同步（`catalog_fetch/import`）、配额管理（`quota_list/refresh/refresh_all/test`）、用量与留痕查询（`usage_*` / `trace_*`）及系统设置接口。

---

## 10. 无头服务端与配额引擎（`crates/server` & `gateway::quota`）

### 10.1 无头服务端（`moonbridge-server`）

`moonbridge-server` 在同一端口挂载 LLM 网关路由、`/api/*` 管理 REST 接口（与 Tauri IPC commands 一一对应）以及前端 SPA 静态文件托管：

- **启动参数**：
  - `--admin-token` / `MOONBRIDGE_ADMIN_TOKEN`（必填）：管理 API 专用 Bearer 令牌，仅驻留内存。
  - `--gateway-token` / `MOONBRIDGE_GATEWAY_TOKEN`：LLM 网关入口令牌，不可为空且不得与管理令牌相同。
  - `--key-file` / `MOONBRIDGE_KEY_FILE`：数据库加密密钥路径（默认 `<db>.key`）。
  - `--addr`、`--config-dir`、`--data-dir`、`--web-dir`（`MOONBRIDGE_WEB_DIR`）。
- **进程内平滑重启**：`POST /api/gateway/restart` 在进程内排空当前网关连接、执行插件 `shutdown`/`init` 钩子并重新绑定端口，同时取消旧实例的配额轮询任务。

### 10.2 配额查询引擎（`gateway::quota`）

- **运行机制**：Provider 绑定 `category = "quota"` 的插件后，引擎按 `provider.endpoints` 顺序对每个 `(base_url, api_key)` 调用一次 `MB.query(ctx)`，将结果按 `(provider_key, key_index)` 写入 `quota_results` 表（API Key 掩码为 `key_label`）。
- **返回结构**：脚本返回 `{ status = "ok"|"error", message?, quotas = { { type, label, ... } }, summary? }`。`type` 支持 `percentage`（百分比）、`quota`（金额/额度）、`counter`（计数器），可附带 `period_secs` 与 `reset_at`。
- **安全边界（`quota_http`）**：配额 HTTP 请求默认禁止访问私网地址与云元数据地址（私网白名单通过环境变量 `MOONBRIDGE_QUOTA_PRIVATE_ORIGINS` 指定精确 origin JSON 数组），禁用重定向与系统代理，固定校验后的 DNS 解析结果，响应体上限 1 MiB，单次请求超时 30s、单 Provider 超时 45s。

---

## 11. 前端架构（`ui`）

基于 Vue 3.5 + Vite 7 + TypeScript 5 + Pinia + Vue Router + TailwindCSS 3 + CodeMirror 6 构建。

- `src/lib/api.ts`：统一契约与 `call()` 分发层，自动适配 Tauri IPC、浏览器 REST（`src/lib/web.ts`）与内存 Mock（`src/lib/mock.ts`）。
- `src/views/`：包含 Dashboard、Providers、Models、Routes、Plugins、Usage、Quota、Traces、Settings 九个核心视图。`RouterView` 使用 `<keep-alive>` 缓存视图状态，切换页面时后台静默刷新数据。
- **TPS 计算口径**：Dashboard 与 Traces 详情页的 TPS 按 `输出总 token（含 reasoning）/ (总延迟 - TTFT)` 计算；生成阶段耗时小于 200ms 的非流式或瞬时单包响应不计入 TPS 统计。
