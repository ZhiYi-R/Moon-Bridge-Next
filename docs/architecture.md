# Moon Bridge Next 架构文档

> 本地 LLM 网关：多协议入口 · Core IR 居中转换 · Lua 插件（可操作出入站原始报文）· 用量统计。
> 技术栈：Rust (edition 2021) + Tauri 2 + Vue 3 + Vite + TypeScript。

本文档描述**已落地实现**的结构与契约，作为二次开发的入口。设计决策的完整推导见仓库计划文档。

---

## 1. 设计原则

- **Core IR 居中**：所有入口协议 → `CoreRequest / CoreResponse / CoreStreamEvent` → 所有上游协议，把 N×M 转换降为 N+M。
- **workspace 多 crate 强制分层**：依赖方向单向，编译期杜绝反向依赖（防止「屎山」式腐化）。
- **hooks trait 解耦**：`protocol` 定义 `PluginHooks` trait，`plugin` 用 mlua 实现它，`gateway` 装配注入；protocol 永不依赖 plugin。
- **Lua 双层钩子**：插件既能操作**协议中立的 Core IR 语义层**，也能直接读写**出入站原始 HTTP 报文**（headers + body + 每个 SSE chunk），位于协议转换最外层，可改写 / 短路 / 中止 / 丢弃。

---

## 2. 分层架构与依赖方向

```
crates/core       moonbridge-core      基础层：Core IR + Protocol + error + modelref（无内部依赖）
crates/protocol   moonbridge-protocol  Adapter traits + PluginHooks(Core+Raw) + Raw 载体 + Registry + 各协议 adapter
crates/plugin     moonbridge-plugin    mlua 运行时 + mb.* 宿主 API + LuaPluginRegistry(impl PluginHooks)
crates/store      moonbridge-store     rusqlite + migration + DAO(provider/model/route/plugin/usage/settings)
crates/gateway    moonbridge-gateway   axum server + 路由 + dispatch 编排 + provider 管理 + usage
crates/server     moonbridge-server    无头服务端：同端口合并 LLM 网关 + /api 管理 REST + SPA 静态托管
src-tauri         moonbridge-app       Tauri 壳：commands(调 gateway/store) + tray + 引导配置
ui                —                    Vue3 前端（Tauri IPC / 纯浏览器 REST 双运行时）
```

**依赖方向（单向，禁止反向）**：

```
core                       (无内部依赖)
protocol  → core
plugin    → core, protocol (实现 protocol 定义的 PluginHooks trait)
store     → core
gateway   → core, protocol, plugin, store
server    → gateway, store, core
app       → gateway, store, core
```

`protocol` 只依赖 `core` 与自己定义的 `PluginHooks` trait；`plugin` 依赖 `protocol` 去实现该 trait；`gateway` 负责把 `plugin` 的 registry 注入 `protocol` 的 adapter。无循环依赖。

---

## 3. Core IR（crates/core/src/ir.rs）

Rust enum + serde，类型安全；mlua 的 `serialize` 特性自动与 Lua table 互转。

```rust
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text { text }, Image { image_data, media_type },
    ToolUse { id, item_id, name, namespace, input, signature }, ToolResult { tool_use_id, content, is_error },
    Reasoning { text, signature, redacted },
}
#[serde(rename_all = "lowercase")] pub enum Role { System, User, Assistant, Tool }
pub struct Message { role, content: Vec<ContentBlock>, ext: Map }
pub struct Tool { name, description, input_schema: Value, ext: Map }

pub struct CoreRequest {
    model, model_alias, system: Vec<ContentBlock>, messages: Vec<Message>, tools: Vec<Tool>,
    tool_choice, max_tokens, temperature, top_p, stop, stream, reasoning, meta: Map,
}
pub struct CoreResponse { id, model, content, stop_reason, usage: Usage, ext }
pub struct Usage { input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, reasoning_tokens }
// 口径不变量：input_tokens 为 prompt 总量（含 cache_read + cache_write），
// output_tokens 为输出总量（含 reasoning_tokens）；cache_*/reasoning 是子集
// 拆分，供按价目分项计费，不另加进总量。各 Adapter 入站归一化须满足该口径
//（如 Anthropic input 并入缓存、Gemini output 并入 thoughtsTokenCount）。

#[serde(tag = "type", rename_all = "snake_case")]
pub enum CoreStreamEvent {
    MessageStart { id, model }, BlockStart { index, block }, BlockDelta { index, delta },
    BlockStop { index }, MessageDelta { stop_reason, usage }, MessageStop, Error { message },
}
```

> **序列化命名约定**：Core IR 内部字段为 `snake_case`（Lua 侧亦按此读写，如 `req.model_alias`、`usage.input_tokens`）；而面向前端的 DTO（store models / GatewayStatus / AppInfo / AppConfig）统一 `camelCase`。

---

## 4. 协议适配层（crates/protocol）

四象限 Adapter（入口 Client / 上游 Provider × 非流式 / 流式），全部 `async_trait + Send + Sync`：

| trait | 职责 |
|-------|------|
| `ClientAdapter` | 入口协议 ↔ Core（非流式）：`to_core_request` / `from_core_response` |
| `ClientStreamAdapter` | Core 事件 → 入口协议 SSE chunk：`encode(ev) -> Vec<RawChunk>` |
| `ProviderAdapter` | Core ↔ 上游协议（非流式）：`from_core_request -> UpstreamRequest` / `to_core_response` |
| `ProviderStreamAdapter` | 上游 SSE chunk → Core 事件：`decode(chunk) -> Vec<CoreStreamEvent>` |

为支持 chunk 级报文钩子，流式被拆为「读 chunk / 解码 / 编码 / 写 chunk」多步，由 gateway 在中间插入 raw 钩子。

`Registry`（`builtin_registry()`）按 `Protocol` 注册各 adapter。**四种协议均实现四象限**，
每个 adapter 同时注册到入口/上游 × 非流式/流式四张表，构成 4×4 全矩阵（任意入口
协议 → 任意上游协议）：

- `adapters/openai_responses/`：OpenAI Responses（`/v1/responses`，入口 + 上游）。
- `adapters/anthropic/`：Anthropic Messages（`/v1/messages`，入口 + 上游；含 system/tools/
  tool_choice 映射、连续同 role 合并）。
- `adapters/openai_chat/`：OpenAI Chat Completions（`/v1/chat/completions`，入口 + 上游；
  入口与上游同格式，共享一套 Chat↔Core 转换）。
- `adapters/google_genai/`：Google Generative AI / Gemini（入口 + 上游；`contents`/`parts`/
  `systemInstruction`/`functionCall`/`generationConfig` 映射，`functionResponse` 按函数名关联）。

> Gemini 无 Anthropic 式显式块边界事件，其上游流式 `decode` 以「首块含 `modelVersion`」
> 合成 `MessageStart` + 文本块起始，末块 `finishReason` 触发收尾；纯函数调用响应会留下
> 一个空文本块（各入口编码器可容忍）。Gemini 入口为次要链路（未挂载 HTTP 路由），
> 主要用于矩阵完整性与测试。

> **推理凭据（加密 CoT / thoughtSignature）的四协议形态**。凭据是 thinking 模式的硬性回传物：
> 多轮历史缺凭据即 400。跨协议时凭据按**来源前缀**标记（`ant:` / `oai:` / `gem:` / `chat:`，
> 见 `adapters/mod.rs` 的 `tag_signature` / `untag_signature` / `emit_signature`）——归属协议
> 解标还原原文，异源协议按 opaque 原样透传，不会以假凭据污染真上游：
>
> - **Responses**：出站无条件补 `include: ["reasoning.encrypted_content"]`（summary 默认
>   `auto`）；凭据在 reasoning item 的 `encrypted_content`（下发给客户端时明文 summary 并存，
>   但**回传上游只带 `encrypted_content`**——明文 summary 是展示产物，异源凭据会被上游拒收
>   故跳过）。
> - **Anthropic**：redacted 推理以 `redacted_thinking` 块承载，凭据在 `data` 字段；入口编码器
>   对推理块**惰性开块**（`StreamEncodeState.open_blocks`，消灭空 thinking 块；凭据先至按
>   redacted 形态开块）。
> - **Gemini**：thought part 的 `thoughtSignature`（含流式末块空 text 搭凭据）。
> - **Chat**：上游无独立凭据字段——**推理明文本体即凭据**，decode 侧打 `chat:` 前缀
>   （payload 为推理原文），使客户端方向（Responses 等）有可回传的不透明凭据；`<mb-cot>…</mb-cot>`
>   搭车 `reasoning_content` 尾部承载**异源**凭据。上游方向：assistant 历史双写
>   `reasoning_content` / `reasoning`（此前 Reasoning 块整体丢弃 ⇒ thinking 上游多轮 tool_calls
>   链必 400），并**合并连续 assistant 消息**——Responses 入口把同一轮展平成相邻 assistant 消息，
>   而 thinking 上游要求推理与 `tool_calls` 落在同一条上。

---

## 5. 双层钩子模型（PluginHooks）

`PluginHooks`（定义于 protocol，plugin 用 Lua 实现，gateway 装配）分两组：

### Core IR 语义层（capability `core`）

| 钩子 | 语义 |
|------|------|
| `on_request(ctx, &mut req)` | 修改 CoreRequest |
| `inject_tools(ctx) -> Vec<Tool>` | 追加工具 |
| `on_response(ctx, &mut resp)` | 修改 CoreResponse |
| `on_stream_event(ctx, &mut ev) -> bool` | 返回 true 丢弃该事件 |
| `filter_content(ctx, &mut block) -> bool` | 返回 true 跳过该内容块。挂载点两处：**非流式**在 `on_response` 之后逐块过滤 `CoreResponse.content`；**流式**只对 `BlockStart` 问询（只有它带完整块），被丢块的 `index` 记入集合，其后续 `BlockDelta`/`BlockStop` 一并压制，避免客户端收到无头孤立增量 |
| `transform_error(ctx, msg) -> String` | 转换错误消息（`dispatch` 在错误响应前调用） |

### 生命周期层（非请求作用域）

`MB.init` / `MB.shutdown` 由 `PluginHooks::init_all` / `shutdown_all` 扇出，归属服务
生命周期（桌面网关 `server::serve_with_shutdown` 与无头服务端 `serve.rs`）：绑定成功后、
开始服务前按加载顺序 init，优雅排空后按逆序 shutdown，重启时下一代重新成对执行。
不放在同步 `bootstrap` 中；绑定失败时也不应留下「init 跑过、shutdown 永不跑」的不配对状态。

- 不经 capability、不经 provider 三态门控：初始化是插件自身的事，与它对哪些请求生效无关。
- 单插件 init/shutdown 抛错只记 warn，不阻断其它插件（与请求链路钩子的容错口径一致）。
- `forget_session(session_id)` 同属这一层：由会话表 LRU 淘汰触发（见 §8），跨插件清掉该会话在
  `SessionStore` 里的桶。它不需要脚本实现——`LuaPluginRegistry` 的实现直接落到它与各插件运行时
  共享的那个 `SessionStore::clear_session`，插件无需感知。

### 出入站原始报文层（capability `raw_request` / `raw_response` / `raw_stream`）

| 钩子 | capability | 语义 |
|------|-----------|------|
| `on_client_request_raw` | raw_request | 入站请求（客户端→网关） |
| `on_upstream_request_raw` | raw_request | 出站请求（网关→上游） |
| `on_upstream_response_raw` | raw_response | 入站响应（上游→网关，非流式） |
| `on_client_response_raw` | raw_response | 出站响应（网关→客户端，非流式） |
| `on_upstream_chunk_raw` | raw_stream | 上游 SSE chunk（流式） |
| `on_client_chunk_raw` | raw_stream | 回写客户端 SSE chunk（流式） |

报文层数据载体（`crates/protocol/src/raw.rs`）：

```rust
pub struct RawMessage { stage, protocol, provider, method, url, status, headers: Vec<(String,String)>, body: RawBody }
pub struct RawChunk   { stage, protocol, provider, event, data: RawBody, raw: String }
pub enum RawBody      { Json{value}, Text{text}, Binary{data}, Empty }
pub enum RawVerdict   { Pass, ShortCircuit{status,headers,body}, Abort{message} }
pub enum ChunkVerdict { Forward, Drop }
```

> **capability 门控即性能开关**：未声明 `raw_stream` 的插件，流式每 chunk **完全不产生 Lua 调用**（零开销）。

---

## 6. Lua 插件系统（crates/plugin）

### 插件形态

脚本执行后暴露全局 `MB` 表，既承载清单也承载钩子；宿主 API 挂在全局 `mb`（小写）。

```lua
MB = {
  version = "0.1.0",
  scopes = { "global" },                 -- global / provider / model / route
  capabilities = { "core", "raw_request", "raw_stream" },
  config_schema = { ... },               -- 供 UI 渲染配置表单（可选）
}
function MB.on_request(ctx, req) ... end -- 就地修改 req 即生效，亦可 return 新 table
```

- **清单**：`Manifest { name, version, scopes, capabilities, config_schema, entry }`。`name` 以 store 记录为权威。
- **运行时**：每插件一个 `mlua::Lua`（`lua54 + async + send + serialize + vendored`），封装为 `Arc<tokio::sync::Mutex<Lua>>` 串行化；利用 Lua table 引用语义实现「就地修改 → 宿主回写」。
- **注册表**：`LuaPluginRegistry` 串联多插件、`impl PluginHooks`，按 capability 门控；单插件钩子出错只记 warn 并跳过，不拖垮请求链路。
- **启用门控 `MB.requires`（仅 app 层）**：脚本可额外声明 `MB.requires = { <网关配置键> = <期望值>, ... }`
  （bool / int / float / 字符串；提取时行级剥掉 `--` 注释，注释里的声明不生效）。`src-tauri`
  的 `plugin_save` 在**启用动作**（新建即启用、或停用→启用）时对照当前网关配置逐项校验，
  不满足则拒绝并返回 `REQUIREMENTS_ERROR_PREFIX` 开头的结构化错误（前端据此弹「设置不满足」
  提示框而非普通错误横幅）；`plugin_import` 不阻断导入——导入后若 `requires` 未满足则**保持停用**
  并在结果 message 里说明。
  `runtime.rs` **不解析** `requires`——网关侧不感知该字段，门控是 app 层策略。
  示例：`plugins/utils/FxxkDax.lua` 声明 `requires = { sessionMarker = true }`，在出站报文层
  注入 `x-opencode-session`（无会话时占位 `no-session`；上游缺该头即 400）。

### 宿主 API（`mb.*` 白名单，crates/plugin/src/host.rs）

| API | 说明 |
|-----|------|
| `mb.log.{debug,info,warn,error}(msg)` | 结构化日志（tracing，target=plugin） |
| `mb.config` | 该插件的 typed config（来自 store） |
| `mb.session.get(k) / set(k,v)` | 会话状态，按「插件名 + session_id」隔离 |
| `mb.http.request{method,url,headers,body,timeout}` | async，经 `HostBridge` 走宿主 reqwest（受 egress proxy/超时管控） |
| `mb.provider.invoke(provider, model, core_request)` | async，跨 provider 编排 |
| `mb.headers.{get,set,remove}(headers, name)` | 大小写不敏感、保序，操作 `{{k,v},...}` |
| `mb.crypto.{sha256,hmac_sha256,base64_encode,base64_decode}` | 上游签名/编码 |

- **沙箱与配额**（`crates/plugin/src/quota.rs` + `host::sandbox`）：把 `os / io / loadfile / dofile / require / package` 六个全局**整体置 nil**（`require` 与 `package` 必须一起移除——留着它们等于留着 `require("io")` / `package.loadlib` 的重取通道），并施加四项硬性配额：
  - **指令计数**：每插件一个 `ExecutionBudget`，经 Lua debug hook（`every_nth_instruction`，默认 step 2000）累加，超 `max_instructions`（默认 2 亿）即中止，掐断死循环。因 Lua debug hook **按 `lua_State`（线程）生效且不被新建线程继承**，需两处补挂：宿主侧 `call_async` 以 `create_thread + Thread::set_hook + into_async` 绑定本次调用的协程；插件侧则注入 `COROUTINE_PATCH` 覆写 `coroutine.create`/`wrap`，线程一诞生即经宿主回调 `__mb_bind_thread` 补挂**同一颗**预算钩子。否则插件内 `coroutine.wrap(function() while true do end end)()` 会在全新无钩线程里死循环，绕过指令配额与超时，并因 `Lua` 互斥守卫跨 await 持有而永久卡死该插件的请求链路。补丁内 `bind` 与 `new_thread` 均为 upvalue，插件覆写 `coroutine.create` 或置空 `__mb_bind_thread` 都无法造出无钩线程。
  - **内存上限**：`Lua::set_memory_limit`（默认 1024 MB），越界分配触发 `MemoryError`。
  - **执行超时**：hook 内附带 wall-clock 截止时间（`call_timeout`），覆盖缓慢（非死循环）的长计算。
  - **body 降级**：raw body 超 `max_body_bytes` 时不展开为 Lua table（置 `nil` + `body_truncated` 标记），回写时保留原始报文，避免超大报文撑爆沙箱。阈值随 `GatewayConfig.max_body_bytes`（默认 100 MB），故大报文展开依赖内存上限（默认 1024 MB）兜底。
  - `mb.http.request` 未显式指定超时则由 gateway bridge 施加兜底超时（默认 30s），防止 raw 钩子内挂死。
  - 配额上限由 `SandboxLimits` 描述，gateway 从 `GatewayConfig` 派生注入。注意 `request_timeout_secs` **只**用于推导插件的 `call_timeout`；上游 HTTP 请求**刻意不设总超时**（长流式请求不应被网关截断），仅受连接与 egress 策略约束。
- **HostBridge**（crates/plugin/src/bridge.rs）：受控宿主能力契约，由 gateway 实现并注入，维持 plugin 不依赖 gateway 的单向依赖。
- **示例插件**：`plugins/examples/log_request.lua`（Core 层）、`plugins/examples/raw_rewrite.lua`（报文层）。二者均有加载执行回归测试（`crates/plugin` 的 `loads_repo_example_plugins`）。
- **脚本引用解析**（`moonbridge_gateway::parse_script_ref`，gateway 加载与 src-tauri 在线编辑**共用同一套规则**）：由 `GatewayConfig.plugins_dir` 决定 `script_ref` 语义——
  - 以 `.lua`（大小写不敏感）结尾 ⇒ 视为**文件**：相对路径拼到 `plugins_dir`，绝对路径必须落在 `plugins_dir` 内，越界返回 `ScriptRef::Rejected`（在线编辑接口据此报错，不再回退读任意路径）；文件不存在/不可读则跳过该插件，**不再静默当成内联脚本文本执行**。
  - 其余 ⇒ 视为**内联脚本**字符串。
  - `plugins_dir` 为空视为未配置（不做文件解析，也不做目录约束）。
  - 归一化用 `canonicalize_pending`：对尚未创建的嵌套路径（`a/b/c.lua`）先回溯到最近的已存在祖先再 canonicalize，从而允许写插件时新建子目录；同时 `..` 穿越与逃逸符号链接仍被拒。
  - `plugins_dir` 以 `AppPaths` 为权威，`start_gateway` 每次启动回写覆盖——防止设置页保存整体 `AppConfig` 时把它清空而静默解除目录约束。

---

## 7. 存储层（crates/store）

SQLite（rusqlite, bundled + WAL），手写版本化 migration，每表一个 DAO 模块，统一由 `Database` 暴露。并发模型：单连接 + `parking_lot::Mutex` 串行化（本地网关低并发足够）。

当前 schema 版本 **V13**（`schema.rs` 的 `MIGRATIONS` 按版本升序手写，`schema_version` 表记录已应用版本；V13 在事务中迁移旧数据并写入加密元数据）。

| 表 | 关键列 |
|----|--------|
| `providers` | key PK, version, user_agent, web_search_json, extra_json, enabled, timestamps。**注意：`protocol` / `base_url` / `api_key_enc` 三列已分别由 V3、V4 删除**，迁至 `provider_endpoints` |
| `provider_endpoints` | provider_key + idx PK, protocol, base_url, api_key_enc —— 一个 Provider 持多端点，**每端点独立协议与独立 API Key**；按 `idx` 升序故障转移 |
| `models` | slug PK, display_name, context_window, **max_output_tokens（V8，可空；models.dev `limit.output`）**, modalities_json, reasoning_levels_json, extra_json。**注意：`pricing_json` 已由 V7 删除**，定价口径统一在 offers |
| `offers` | provider_key + model_slug PK, pricing_json, **endpoint_protocol（V6，可空）** —— 非空时该模型只走 provider 中匹配该协议的端点，为空则全端点按 idx 故障转移 |
| `routes` | alias PK, model_slug, provider_key, extra_json |
| `plugins` | name PK, source(lua), script_ref, enabled, config_json, scopes_json, capabilities_json |
| `plugin_bindings` | plugin_name + scope + scope_key PK, enabled, config_json |
| `usage_records` | id PK, session_id, model, upstream_model, **provider_key（V9，可空）**, input/output/cache_read/cache_write/**reasoning**_tokens, cost, status, error, latency_ms, **ttft_ms**, created_at |
| `settings` | key PK, value_json |
| `balance_cards`（V10） | key PK, api_key, base_url, provider_label, script_ref（规则同插件：`.lua`=plugins_dir 内文件，否则内联脚本）, interval_secs（0=禁用，保存时 1..59 夹到 60）, enabled, extra_json, position, timestamps；V11 加 provider_key/display_mode |
| `balance_results`（V12 重建） | (card_key, key_index) 联合 PK, key_label（掩码 key 标签，原文不落此表）, status(ok/error), payload_json（该 key 最近一次返回，引擎级失败时保留旧值）, error, queried_at —— 多 key 卡片按 key 拆行，删卡同事务级联 |

- 凭据加密：默认文件数据库通过 `EncKey` 对 Provider 与余额查询凭据统一使用 AES-GCM，V13 在事务中迁移旧数据与加密元数据；不再以明文实现作为默认文件存储。字段加密不等于整库、配置、脚本或 trace 加密，前端编辑/查询仍可能处理凭据。
- 历史自定义 `EncKey` 的 Provider 数据迁移须调用 `Database::open_with_legacy_key(path, target, legacy_provider_key)`，明确旧解密器后再加密，不能猜测密文前缀；旧余额凭据按明文迁移。解密或写入失败会回滚凭据及加密元数据；默认文件入口针对旧服务的明文存储迁移。
- 密钥默认 `<db>.key`，服务端可用 `--key-file` / `MOONBRIDGE_KEY_FILE` 指定；Unix 权限 `0600`，Windows 以当前用户 DPAPI 包装，恢复受该账户绑定限制。已有加密数据缺少/错误密钥时拒绝打开，不回退明文。
- 数据库和密钥必须成对备份。升级前另存数据库备份；旧镜像不能直接使用新加密数据库回滚，必须恢复升级前数据库及匹配的凭据材料。
- `status` 取值 `ok` / `error` / `aborted`（流未读尽即结束，如客户端断开）；`ttft_ms` 仅流式请求有值。
- `cost` 由 `usage::record` 在落库时按 `(provider_key, upstream_model)` 命中的 offer
  `pricing_json` 现算（价目单位 USD/1M tokens，键 `input`/`output`/`cache_read`/
  `cache_write`/`reasoning`）。Core 口径里 cache_* 与 reasoning 分别是 input/output
  的子集：先按 input 价拆出 `input − cache_read − cache_write`、按 output 价拆出
  `output − reasoning`，缓存按各自价、reasoning 缺省回退 output 价、cache_* 缺省
  回退 input 价。**存的是当时价**——价目后续变动不回改历史账单；无定价/定价缺失
  时 cost 为 0（与免费模型同值，不区分）。
- 长上下文分层计价：pricing 可含 `tiers`（models.dev 权威形态，`tier.size` 为
  context 阈值）或旧式 `context_over_200k` 镜像；`input_tokens` 超过阈值后整档
  价键逐项覆盖基价（未列价键回退基价）。目录导入保留两种形态。
- trace 大对象存文件系统 `app_data_dir/traces/<session>/<model>/<created_at>-<short_id>.json`（`short_id` 为 request_id 首段），不入库。
- trace 的 `upstreamResponse`/`clientResponse`：**非流式**记协议响应体原文；**流式**记 `StreamAssembler` 从事件流聚合出的最终消息（`CoreResponse` 形态，见 §8）。
- **trace 治理**：`trace_record_bodies`（默认 `true`）为 `false` 时在 `dispatch::finish_audit` 收口统一抹体——`clientRequest`/`clientResponse`/`upstreamResponse` 置 null，`upstreamRequest` 只清 `body`（它的 URL/headers 已脱敏，但 body 含完整 prompt；只清 `clientRequest` 会留下旁路，流式路径曾因此泄漏）。`trace_retention`（默认 `500`；`0` = 关闭整理）按 mtime **全局** prune（跨会话/模型目录）并清掉空目录。快照脱敏在 `dispatch` 构造时完成：头名含 `auth`/`api-key`/`apikey`/`token`/`cookie`/`secret` 的值整体替换为 `[REDACTED]`；URL 查询参数名含 `key`/`token`/`secret` 的值同样脱敏（Google 风格 `?key=`）。上游非 2xx 的 `trace.error` 只记 `上游返回 HTTP {status}` 摘要，HTML 错误页不再整页入库。

---

## 8. 网关请求生命周期（crates/gateway）

```
Client → axum: POST /v1/responses | /v1/messages | /v1/chat/completions
  → 认证(Bearer) / session 解析(session_id / previous_response_id / X-Codex-Window-Id)
  → [RAW] on_client_request_raw            改 headers/body，可 short_circuit / abort
  → 按入口路径选 ClientAdapter → to_core_request(raw) → CoreRequest
  → 会话水印：就地剥除请求内全部 marker → 解析/分配 ctx.session_id（见下）
  → 路由解析: alias → (provider, 上游 model) → Provider + 上游 Protocol
  → [CORE] on_request / inject_tools
  → 选 ProviderAdapter → from_core_request → UpstreamRequest(headers+body)
  → [RAW] on_upstream_request_raw          改上游 method/url/headers/body（全部回读，见下）
  → reqwest 发送(受 egress proxy)
  → [流式] 逐 chunk:
             [RAW] on_upstream_chunk_raw → [CORE] decode + on_stream_event
             → [CORE] filter_content（丢块连带压制同 index 增量；水印注入首个
               推理块的首部增量）
             → [CORE] encode → [RAW] on_client_chunk_raw → 回写入口 SSE(axum Sse)
     [非流式] [RAW] on_upstream_response_raw → to_core_response → [CORE] on_response
             → [CORE] filter_content → 嵌入会话水印（首个推理块明文首部）
             → from_core_response → [RAW] on_client_response_raw → 回写
  → usage 落库 + trace 落盘（若配置 trace_dir）
```

> **会话水印（`crates/gateway/src/session.rs`）**：把 marker 嵌进助手**推理（CoT）块**的
> 明文首部（`" [mb:<载荷>]"`），客户端下一轮把整段历史带回来时即可认回同一会话。
> 存在的理由：Qwen Code 这类客户端在请求头与请求体里都不带会话标识（其 `prompt_cache_key`
> 注入路径以 hostname 恰为 `api.openai.com` 为前提，经网关时不生效），于是 `ctx.session_id`
> 恒为 `None`，插件的 `mb.session` 与 trace 的会话目录一起失去粒度。
>
> - **为什么嵌 CoT 而非正文**：thinking 回传是硬语义——thinking 模式上游缺
>   `reasoning_content`/thinking 块直接 400，客户端必然原样带回，与 `<mb-cot>` 凭据机制
>   同一条可靠通道；且每个 thinking 响应都带推理块（含 tool_use 轮），首轮起即可打标。
>   正文是用户可见输出、且会被客户端包进 tool_result 等结构——marker 不再污染它。
> - **入站即剥除，剥完才往下走**：`extract_from_request` 在 `to_core_request` 之后、路由与
>   插件 `on_request` 之前原地改写 `CoreRequest`，覆盖 `Text` / `Reasoning.text` /
>   `Reasoning.signature`（`chat:` 自凭据的载荷就是推理明文）与 `ToolResult` 子块。
>   上游模型的输入里永远不出现 marker，因此不存在「模型模仿 marker」的可能，marker 的唯一
>   存活期是客户端本地 transcript。trace 的 `clientRequest` 记剥除**前**的客户端原样、
>   `upstreamRequest.body` 记剥除**后**的转发内容，两者对照即是这条不变量的证据。
> - **载荷即 session id**：网关自分配会话的 marker 载荷就是 session uuid 本身——
>   `ctx.session_id` 由载荷直接还原，活跃表淘汰 / 网关重启都不再改判身份
>   （`x-opencode-session` 等下游亲和头不漂移，这正是长对话缓存失效的修复点）。
>   外部身份（body `session_id` / `previous_response_id` / `X-Codex-Window-Id`）的载荷是
>   `tag_from_id` 派生的定长 6-hex 短 tag，经 `SessionTable`（`载荷 → id`，LRU 淘汰，
>   表深 `session_table_depth` 默认 64，`<1` 钳到 1）还原；表里查不到的短 tag 合成
>   确定性 `mb-{tag}`——同一 tag 反复回带恒落同一会话。短 tag 撞车时换新 tag 而非
>   顶掉已有会话。
> - **解析优先级**：外部显式身份 > marker 载荷 > 新分配。
> - **两处打标**：非流式 `tag_response` 把 `" [mb:<载荷>]"` 插进首个非 redacted 推理块的
>   明文首部（前置空格剥除时连带吃掉，打标→剥除字节还原）；流式在首个可承载推理块的
>   `BlockStart`（或首个裸推理增量）处向**同 index** 注入一条推理明文增量，marker 成为
>   该 thinking 块首部——不新占块、不动块序、不拦截 `BlockStop`。redacted / 凭据先行的
>   块不可注入（入口编码器会把它开成 `redacted_thinking` 完整块）。无推理块的轮次不打标
>   （纯 CoT 方案：不向正文注水、不凭空造块），身份顺延到下一个含推理块的响应。
>   打标块的 `BlockStop` 若携带完整块（chat 上游的收尾块、responses 的 done item 组装料），
>   其明文同样补上 marker 首部，保证只存完成态的客户端也能回带水印。
>   被剥成空壳的幻影块（marker 独占、无凭据）连同删除；保底不把 `content` 删成空数组。
> - **淘汰即清理**：被 LRU 挤出的 session id 交回 `dispatch::forget_sessions` →
>   `PluginHooks::forget_session` → `SessionStore::clear_session`，回收插件侧 `mb.session` 的桶。
> - **代价与关闭**：每轮多约十余 token，marker 出现在客户端 transcript 的 thinking 明文里
>   （正文不可见）。`session_marker = false` 时不再嵌入，但**入站 marker 照剥**（客户端
>   可能带着开启期间留下的历史，不该污染上游 prompt）。已经自带会话标识的客户端
>   （走 `session_id` 字段或 `X-Codex-Window-Id`）可直接关掉。

> **流式收尾由 `StreamAudit::Drop` 承担**，而非写在读流循环之后。流可能以三种方式结束——
> 正常读尽（`ok`）、中途出错并已下发带内 error 事件（`error`）、客户端断开或上游提前关闭
> （`aborted`）；只有 Drop 能同时覆盖，尤其是客户端断开时整个生成器被直接丢弃、循环后的代码
> 根本不会执行。解码错误与**编码错误**都以 `'stream` 标签跳出整条流（编码错误原先只 break 内层
> `for`，会反复刷 error 事件）；raw chunk 钩子返回 `Err` 时上下游侧对称地记 `warn` 后放行，
> 不再被 `if let Ok(..)` 静默吞掉。
>
> **流式响应体由 `StreamAssembler` 聚合落盘**：trace 的两个响应字段不再是 `null`——
> `upstream_response` 喂入 decode 后、Core 钩子**前**的事件（上游实际发送的内容），
> `client_response` 只喂入实际编码下发的事件（客户端实际收到的内容，含插件过滤与
> 会话水印注入的效果）；两者对照即可看出插件对流的改写。聚合以最终消息体为上界
> （裸 delta 自动建占位块、`ToolUse` 的 `partial_json` 拼串后一次 parse、`BlockStop`
> 携带的完整块优先），不存逐 chunk 时序；`record_bodies=false` 时照常抹除。

> **出站改写经 `dispatch::apply_outbound` 全量回读**：`method` / `url` / `headers` / `body`
> 四项都写回 `UpstreamRequest`。历史上 `method` 只写进递给 Lua 的表却从不回读（改它无效），
> `body` 只认 `RawBody::Json`（改写成 `Text` 被静默丢弃）。上游走 `reqwest::json()` 单一通道，
> 故 `Text` 需为合法 JSON 才采用，`Binary` 与非 JSON 文本**显式报错**而非假装成功；`Empty`
> 表示未改写，保留 Adapter 原 body。钩子返回 `ShortCircuit` 会中止整轮端点故障转移（视作
> 插件已决定直接应答，属既定取舍）。

> **短路 / 中止一律先落审计再返回**（`dispatch::answered` / `aborted` → `finish_audit`）。
> 全部 8 个 `ShortCircuit`/`Abort` 返回点（入站请求、出站请求、上游响应、客户端响应各两处）
> 都会写一行 usage + 一份 trace——历史缺陷是这些路径直接 `return`，插件代答或被插件拒绝的请求
> 在用量统计与 Traces 页完全不可见。取值口径：trace 侧 `status` 记 `short_circuit` / `aborted`
> （说明是**谁**答的、答成什么样），usage 侧则按客户端视角记（`2xx` 短路 ⇒ `ok`，其余 ⇒ `error`）。
> 路由之前的短路只有请求侧信息，trace 的上游字段留空（`pre_route_trace`）。

模块划分：`server`（axum 路由/启动，含 `serve_with_shutdown` 优雅关闭）、`dispatch`（编排 RAW/CORE 两组钩子）、`stream`（SSE 编排）、`router`（别名解析）、`session`（会话水印编解码 + 活跃会话 LRU 表）、`upstream`（reqwest 客户端 + SSE 读取）、`bridge`（`HostBridge` 实现）、`usage`（统计落库）、`trace`（报文快照落盘）、`state`（`AppState`，含 `sessions: SessionTable`）、`config`（`GatewayConfig`）。

顶层入口：`bootstrap(config, db) -> Arc<AppState>`（装配内置 adapter、加载插件、构建 HTTP 客户端）、`serve` / `server::serve_with_shutdown`。

---

## 9. Tauri 应用层（src-tauri）

- **引导配置**：`app_config_dir/config.toml` → `AppConfig { gateway: GatewayConfig, logLevel, autoStart }`。其余业务配置全部入 SQLite。
- **状态**：`ManagedState { db, paths, config, gateway }`，网关以 tokio task + oneshot 优雅关闭信号驱动启停。
- **`gateway` 句柄锁的并发约束**：`start_gateway` / `stop_gateway` 等命令必须**避免在持有 `gateway` 互斥锁的同时 `.await`**，也不能在已持锁的路径上再取一次同把锁——`gateway_start` 被重复调用时会自锁死（前端连点即触发）。现在由 `has_live_gateway()`（短临界区，仅判断句柄存在且 `task` 未结束）先行幂等返回，`status()` 也在**一次**加锁内同时读出 running 与 addr。回归测试 `start_gateway_while_running_does_not_deadlock` 把风险调用放进**独立 OS 线程 + 独立 current-thread runtime**、用 `mpsc::recv_timeout` 断言，因为 `tokio::time::timeout` 与被阻塞的 future 同属一个任务、计时器永远得不到轮询，无法用来证死锁。
- **commands**（前端 `invoke`）：
  - 网关：`gateway_start / stop / restart / status`
  - CRUD：`provider_* / model_* / offer_* / route_* / plugin_*（含 `plugin_read_script` / `plugin_write_script` 在线编辑）/ binding_* / usage_* / settings_*`
  - 模型目录：`catalog_fetch`（后端 reqwest 拉取 `models.dev/api.json`，解析精简为扁平候选列表）/ `catalog_import`（勾选批量导入：模型**元数据** upsert 到 `models`，**定价**经 `insert_offer_if_absent` 写入对应 provider 的 offer，并对同 slug 已有空定价的行回填 `backfill_offer_pricing`；刻意不触碰 provider 端点配置）
  - Trace：`trace_list / trace_read / trace_delete`（只读浏览 `trace_dir`，含路径穿越校验）
  - 应用：`app_info / config_get / config_set`
- **托盘**：显示主窗口 / 一键启停网关 / 退出；左键单击显示窗口。
- **插件**：single-instance（桌面，最先注册）、log、process、opener、dialog、window-state。
- **日志**：Rust 侧 tracing-subscriber（`EnvFilter`，默认 info）；前端 console 由 tauri-plugin-log 承接，两者互不冲突。

---

## 10. 无头服务端（crates/server）

`moonbridge-server` 是把桌面端管理能力以 REST API 暴露的无头（headless）形态：**同一端口**
合并三块——LLM 网关路由（`/v1/*` 等，复用 `gateway::server::router`）、`/api/*` 管理 REST、
前端 SPA 静态托管。目标场景是服务器/Docker 部署（无 Tauri、无系统依赖），管理界面改由
纯浏览器访问。

### 启动参数（`args.rs`）

| 参数 | 环境变量 | 语义 |
|------|----------|------|
| `--admin-token <T>` | `MOONBRIDGE_ADMIN_TOKEN` | **强制**，管理 API 专用；缺失拒绝启动 |
| `--gateway-token <T>` | `MOONBRIDGE_GATEWAY_TOKEN` | LLM 入口专用，未提供时读取既有配置；空值或与管理 token 相同均拒绝启动 |
| `--key-file <P>` | `MOONBRIDGE_KEY_FILE` | 数据库加密密钥文件，默认 `<db>.key` |
| `--addr <A>` | — | 监听地址覆盖（默认取配置文件） |
| `--config-dir <D>` | — | 配置目录（默认与桌面端同位置） |
| `--data-dir <D>` | — | 数据目录：数据库、插件、trace（默认与桌面端同位置） |
| `--web-dir <D>` | `MOONBRIDGE_WEB_DIR` | 前端静态文件目录（默认 `./ui/dist`） |
| — | `MOONBRIDGE_BALANCE_PRIVATE_ORIGINS` | 启动时读取的私网余额授权列表：精确 origin 的 JSON 数组；不是卡片 `extra` |

未知参数与位置参数一律报错；`--key value` 与 `--key=value` 均接受。

### 认证模型（管理与网关 token 分离）

- `/api/*` 全部要求 `Authorization: Bearer <admin-token>`，缺失/错误返回 401 JSON；管理凭据不保存、不回显，不能用于 LLM 入口。
- LLM POST 入口使用独立 gateway token。有效 token 来自启动参数/环境变量或既有配置，不能为空或等于 admin token。
- 从旧版共享 token 升级时，旧值迁入 `MOONBRIDGE_GATEWAY_TOKEN`，另生成不同的管理 token；模型客户端不改 key，浏览器使用新管理凭据。
- `GET /api/config` 返回的网关 `authToken` 为 `null`。`PUT /api/config` 传 `null` / 空值保留既有网关 token；显式更新可将新网关 token 落盘。无关保存不会把环境覆盖值写入配置，管理 token 始终不落盘。
- `/health` 与 `GET /v1/models` 继续公开，这是既有行为；SPA 静态资源公开，页面内的数据请求仍要管理 token。

### 进程与生命周期语义

网关随进程运行，Web 前端不展示桌面式进程启停：

- `GET /api/gateway/status` 报告真实运行状态与实际绑定地址，而非只回显待应用的配置地址。
- `POST /api/gateway/restart` 在进程内触发优雅排空，等待请求收尾并执行生命周期钩子，再装配并重新监听；不退出进程、不依赖 Docker/systemd 等 supervisor。容器 restart 策略可用于异常退出恢复，但不是管理重启的实现。
- 插件 `MB.init` / `MB.shutdown` 成对执行：绑定成功后、服务前 init，排空服务后 shutdown；SIGTERM 同样走优雅关闭。
- 每代运行实例只持有一个余额调度器；重启取消旧代调度及其任务，再启动新代，避免后台查询叠加。

### 管理 API（`/api/*`，`admin/`）

与 src-tauri commands **1:1 同语义**（40+ 端点），DTO 直接复用 store/gateway 的
camelCase 契约，前端 `call()` 分发层据此在 IPC 与 REST 间透明切换：

| 分组 | 端点 |
|------|------|
| 网关 | `GET /api/gateway/status` · `POST /api/gateway/restart` |
| 应用 | `GET /api/app/info`（`mode: "server"`）· `GET/PUT /api/config` |
| Provider/Offer | `GET/PUT /api/providers` · `GET/DELETE /api/providers/:key` · `GET /api/providers/:key/offers` · `PUT /api/offers` · `DELETE /api/providers/:key/offers/:slug` |
| 模型/目录 | `GET/PUT /api/models` · `GET/DELETE /api/models/:slug` · `GET /api/catalog` · `POST /api/catalog/import` |
| 路由 | `GET/PUT /api/routes` · `GET/DELETE /api/routes/:alias` |
| 插件 | `GET/PUT /api/plugins` · `GET/DELETE /api/plugins/:name` · `POST /api/plugins/import`（JSON `{files:[{name,content}]}`，不是 multipart）· `GET/PUT /api/plugins/:name/script` · `GET /api/plugins/:name/bindings` |
| 绑定 | `GET/PUT /api/bindings` · `DELETE /api/bindings/:pluginName/:scope/:scopeKey` |
| 用量 | `GET /api/usage` · `GET /api/usage/summary` |
| 余额 | `GET/PUT /api/balance/cards` · `DELETE /api/balance/cards/:key` · `POST /api/balance/cards/:key/refresh`（可加 `?key_index=N` 只刷新单个 key） · `POST /api/balance/refresh` · `POST /api/balance/test`（dry-run） |
| Trace | `GET /api/traces` · `GET/DELETE /api/trace`（单数，路径经查询参数，含穿越校验） |
| 设置 | `GET /api/settings` · `GET/PUT/DELETE /api/settings/:key` |

桌面端的约束在服务端全部保留：`plugin_save` 的 `MB.requires` 启用门控（不满足返回
`REQUIREMENTS` 前缀结构化错误）、脚本读写的路径穿越防护（与 gateway 加载共用
`parse_script_ref` 规则）、trace 读取的路径校验、catalog 导入的定价回填。
`/api/*` 下未匹配的路径返回 JSON 404，不会被 SPA fallback 吞掉。

### 余额&健康看板（`gateway::balance` + `/api/balance/*`）

每张余额卡片是一段**一次性 Lua 脚本**（不进插件注册表、不参与请求链路）：宿主加载脚本
（复用 `LuaRuntime` 沙箱与全部 `mb.*` 宿主 API），调用 `MB.query(ctx)` 并把返回值落库。

- **ctx**（单 key 形状）：`{ name, key, keys, base_url, provider, extra }`（`extra` 为卡片
  自定义 JSON，同时也作为 `mb.config`）；`keys` 恒为当前 key 的单元素数组。
  `mb.http.request` 的响应 body 在上游返回 JSON 时已自动解析为 Lua table，脚本可直接
  `r.body.xxx`。
- **卡片 Key 来源**：新增/编辑支持手动单 Key 或换行分隔的 Key 列表，沿用
  `api_key`（REST/IPC `apiKey`）字符串存储，不改变表结构。按行去首尾空白、忽略空行、
  **去重保序**后非空则完全覆盖服务引用，不读取或混入 Provider 的 Key；此时
  `provider_key` 可选，只作分组和 `ctx.provider` 的默认标签，引用失效也不影响手动查询。
  手动输入为空时才从 Provider 端点解析 Key（空端点继承前一个非空 Key）并去重，
  Provider 不存在则报引擎级错误。UI 保存/测试要求手动 Key 或服务引用至少一项；
  后端保留两者均空时用空 Key 执行一次的公开查询兼容性。
  **每个 Key 各执行一次脚本并拆成多张卡片展示**，`ctx.key` 与单元素 `ctx.keys`
  契约不变，保存、测试拉取、单 Key 刷新和定时查询采用相同优先级。
- **结果按 key 拆行落库**（V12）：`(card_key, key_index)` 联合主键，每行带
  `key_label`（掩码展示标签：长度 ≤4 → `****`，≤10 → 前 2 位 + `…`，更长 →
  前 6 位 + `…` + 后 4 位；key 原文不进结果表）。整卡执行后 `key_index >= 当前 key 数`
  的残留行被剪掉（prune）；卡片「上次查询时刻」取各行 `queried_at` 的最大值（到期判定
  口径）。前端逐 key 渲染一张卡（标题 = 卡片名 + 掩码 key chip），可整卡刷新也可
  单 key 刷新（REST `?key_index=N` / IPC `keyIndex`）。
- **查询 URL 是卡片自己的可选输入**（`base_url`，不从 Provider 端点继承）：配额接口
  基准地址，脚本读 `ctx.base_url`，留空为空字符串。
- **返回契约**：`{ status = "ok"|"error"（缺省=ok）, message?, quotas = { { label,
  used_percent?, left_percent?, unit?, used_amount?, left_amount?, reset_at? } },
  summary?, ... }`。返回 `nil` 或非法 `status` 均为错误，不计为成功。落库前 `quotas`
  归一化为 camelCase 并补齐 used/left percent 互补值（amount 字段原样透传，amount 之间
  及与 percent 之间均不做互补互推；None 字段不序列化）；`reset_at` 接受展示字符串或
  unix 秒数字（归一为字符串，前端对纯数字按本地时间格式化——沙箱无 os 库，脚本拿到
  毫秒时间戳只能除 1000 后给数字）；脚本自定义字段原样保留在 payload。
- **内置脚本模板**（`ui/src/lib/balanceTemplates.ts`）：新建或编辑卡片时可从模板填充脚本与建议
  默认值（查询 URL/额外参数/间隔）；应用模板会覆盖当前编辑中的脚本并切换到内联来源
  （弹窗内已明示「覆盖脚本并切换到内联来源」）。模板按各服务真实接口编写：通用百分比/金额骨架 +
  new-api（`/api/user/self`，当前 `data.quota / 500000` 作美元余额，不用 `used_quota`
  代替余额）、DeepSeek、Moonshot/Kimi 开放平台、SiliconFlow、
  OpenRouter（credits + key 限额双接口）、智谱 GLM Coding Plan（裸 key 鉴权 + Bearer 回退）、
  Kimi Coding Plan（`usages.limit_5h/limit_7d` 的 `used_ratio * 100`、`reset_time`；允许
  仅一个窗口可用，不因另一窗口缺失而丢弃可用结果）、
  CommandCode（完整 URL `/alpha/billing/credits`；月度 `monthlyCredits` 剩余额度，
  月上限默认 70；5H/Weekly 的 used/cap 金额与毫秒 resetAt，单位默认空）、Claude Code
  订阅（OAuth usage 接口）、Sub2API（Bearer `GET /v1/usage`；优先 `quota.remaining`，
  再回退 `balance` / `remaining`，附带 `rate_limits` 的周期剩余额度，单位 $；站点 URL
  自动去尾斜杠及 `/v1`）。模板不携带账户密钥；应用模板只覆盖当前编辑中卡片的脚本，不改动其它已保存卡片。
- **弹窗错误反馈**：新增/编辑表单的校验、保存失败和测试拉取错误显示在弹窗标题下，
  通过 Modal 的可选 `notice` 插槽置于滚动内容区外，始终可见；多 Key 错误保留掩码标签，
  支持脚本 `payload.message`，详情仍在测试预览。页面级刷新/删除错误独立展示。
- **两种配额模式与显示切换**：百分比模式给 percent 字段，显示「{used}% · 剩余 {left}%」；
  金额模式给 `unit` + `used_amount`（可再带 `left_amount`），显示
  「消耗 {used}{unit} · 余额 {left}{unit}」。进度条比例：优先 percent，否则双 amount 时
  `used/(used+left)*100`，都没有则不渲染。卡片 `display_mode`（`auto`/`percent`/`amount`，
  保存时非法值归一为 auto）让用户在卡片上与编辑表单里切换显示样式：auto 按数据自动，
  强制模式缺对应字段时显示「—」不渲染进度条。前端按 `providerKey || providerLabel(旧卡)`
  分组展示卡片。
- **dry-run 预览**：`POST /api/balance/test`（Tauri `balance_card_test`）以请求体里的
  卡片配置逐 key 试跑脚本（卡片无需已保存），返回结果数组，**不写库、不读历史
  结果**——编辑表单的「测试拉取」据此实现预览，引擎级失败时该 key 的 payload 为 None。
- **失败分层（按 key 独立）**：引擎级失败（脚本读不到 / 无 `MB.query` / 抛错 / 超时 /
  返回 `nil` 或非法 `status`）→ 该 key `status=error` 且保留旧成功 payload；有效 table
  （含业务 error）更新 payload。整卡执行预算为 45s。`BalanceBridge` 仅实现 HTTP，
  `provider_invoke` 直接拒绝；一次性入口为 `LuaRuntime::call_mb_once`，复用沙箱与配额。
- **余额 HTTP 的 SSRF 边界**（`gateway::balance_http`）：只允许同源或已授权 origin；
  默认拒绝私网与云元数据目标。私网例外由启动环境 `MOONBRIDGE_BALANCE_PRIVATE_ORIGINS`
  的精确 origin JSON 数组授予，例如 `["https://balance.internal.example:8443"]`，不是
  卡片 `extra`；元数据地址始终禁止。HTTP 不继承系统代理、不跟随重定向，解析并校验
  DNS 后钉住目标地址，解压后响应上限 1 MiB，单次 HTTP 最长 30s。
- **代理限制**：显式 `egressProxy` 导致余额 HTTP 明确报不支持代理，不静默直连；
  网关推理请求的代理支持不受此限制影响。
- **定时调度**：每代运行实例仅持有一个 `spawn_balance_scheduler`，重启取消旧任务。
  每 30s 扫描 `enabled && interval_secs>0 && 已到期` 的卡片串行执行；单卡失败/panic
  不影响循环。`interval_secs` 保存时夹逼：≤0 禁用，1..59 抬到 60。
- **script_ref** 与插件同一套 `parse_script_ref` 规则（`.lua` 收敛到 `plugins_dir` 内）。
- IPC 侧 commands `balance_card_* / balance_refresh_all` 与 REST 1:1（`balance_card_refresh`
  带可选 `keyIndex`）；卡片与逐 key 结果以 `BalanceCardView`（卡片 flatten +
  `results: BalanceKeyResult[]`）成对返回，前端一次拿全。

### SPA 静态托管

`tower_http::ServeDir` + fallback 到 `index.html`（前端 hash/history 路由接管深链）；
`web_dir` 不存在时只告警跳过，网关与管理 API 照常可用。页面提供 CSP 防护。

### 前端 web 运行时（`ui/src/lib/web.ts`）

`api.ts` 的 `call()` 三分发：Tauri 环境走 IPC → `VITE_USE_MOCK=1` 走内存 mock → 其余
（纯浏览器）走 REST。REST 客户端仅在页面内存保存管理 token，不写 localStorage，刷新
需重登；401 时清除并派发 `mb:unauthorized` 事件，`App.vue` 弹出 `LoginCard`。
web 模式隐藏进程启停按钮、保留进程内「重启网关」；插件导入由 `<input type=file>`
读取文本，再以 JSON `{files:[{name,content}]}` 提交，不使用 multipart。

---

## 11. 前端（ui）

Vue 3.5 + Vite 7 + TS 5 + Pinia + Vue Router + TailwindCSS 3 + shadcn-vue 风格组件（radix-vue + cva）。

- `src/lib/api.ts`：前后端契约层（DTO 类型 + command 封装，按领域分组）。
- `src/stores/`：Pinia（gateway 状态、provider 列表）。
- `src/router`：hash 路由（Tauri 自定义协议友好）。
- `src/views/`：Dashboard（网关状态 + 用量 + Provider 概览）、Providers（完整 CRUD；可用模型选择器先列已选 chips，候选列表在搜索框聚焦时才展开下拉）、Models（模型 CRUD + 从 models.dev 搜索勾选批量导入 + provider 维度 Offer 管理，Offer 可绑定端点协议；两区可拖拽分栏）、Routes（别名 CRUD + 必填校验 + 可搜索模型下拉）、Plugins（在线脚本编辑 + 启停/增删 + 一键重启网关生效）、Usage（汇总卡片 + token 时序堆叠柱图 + 模型分布 Top 3 + 其他聚合 + 明细表，纯CSS/SVG 无额外依赖）、Balance（余额&健康看板：卡片引用上游 Provider 或手动 Key 列表（手动优先）+ 可选查询 URL + 新建时可从内置模板库填充脚本（new-api 中转/DeepSeek/Moonshot/SiliconFlow/OpenRouter/智谱/Kimi Coding Plan/Claude Code 等真实接口模板）+ 多 key 在同一卡片内逐行展示（掩码 key chip + 单 key 刷新）+ 按提供商分组 + 百分比/金额两种模式与卡片级显示切换 + 状态徽章 + 一键刷新 + 单列弹窗内测试拉取预览（逐 key 结果）+ 带编写指南的 Lua 脚本编辑）、Traces（主从布局 + 可拖宽列表 + ↑↓ 键盘导航 + 各阶段报文只读高亮，超 256KB 回退纯文本 + 删除）、Settings（网关分区默认展开）。
- `src/components/ui`：Button / Badge / Card / Input / Label / Modal（动画 + dirty 守卫）/ Select / Switch / Pagination / ToastHost / CodeEditor（CodeMirror 6）等，精简 shadcn 风格。
- `src/composables/`：`useConfirm`（Promise 化确认弹窗）、`useToast`（全局通知）、`useAutoPageSize`（实测行高分页 + 页首行锚定防漂移）、`usePointerDrag`（拖宽/分栏共用）。
- 反馈与自适应：保存/删除统一走 toast；网关状态 4s 轮询；列表区分加载态与空态；表单网格 `auto-fit minmax` 随窗口宽度换列。
- **点击响应优化**：`RouterView` 整体 `<keep-alive>`——二次进入页面立即渲染缓存的列表/滚动/筛选状态，后台**静默重拉**（不闪 loading）；各视图独立请求一律 `Promise.all` 并行（Dashboard 三段瀑布合一）；写操作成功后用响应值原地更新，避免整表重拉（仅后端可能改写他字段时保留重拉并注释）。

---

## 12. 构建与运行

前置：Rust 1.85+（注意：锁定的依赖树实际要求 ≥1.88，Docker 构建固定用 rust:1.95）、Node 20+/pnpm、Tauri 2 的 Linux 依赖（webkit2gtk-4.1、librsvg2 等；仅桌面端需要，server 形态无系统依赖）。

```bash
# 后端：编译 + 测试（全 workspace）
cargo check --workspace
cargo test  --workspace

# 前端：安装 + 构建
pnpm --dir ui install
pnpm --dir ui build

# 前端：纯浏览器开发（无需 Tauri 壳，默认连本地真实服务端）
pnpm --dir ui dev
# 浏览器访问 http://localhost:5173；vite 把 /api 代理到 127.0.0.1:38440，
# 页面先弹登录卡填 admin token。VITE_USE_MOCK=1 pnpm --dir ui dev 则切内存
# mock 数据（不连后端），header 出现琥珀色「MOCK 数据」徽标

# 无头服务端（仅本地开发示例；生产需两个不同的随机 token）
cargo run -p moonbridge-server -- --admin-token dev-admin-token --gateway-token dev-gateway-token
# 同端口提供网关 + /api/* + ./ui/dist 静态托管

# 桌面应用（开发热重载）：从项目根运行
cargo tauri dev

# 打包
cargo tauri build

# Docker（先在启动环境提供两个不同的非空随机 token）
docker build -t moonbridge-next:local .
docker run -d -p 38440:38440 \
  -e MOONBRIDGE_ADMIN_TOKEN -e MOONBRIDGE_GATEWAY_TOKEN \
  -e MOONBRIDGE_KEY_FILE=/data/moonbridge.key \
  -v "$PWD/data:/data" --restart unless-stopped moonbridge-next:local
# 数据库和密钥必须一起备份；回滚需要升级前数据库及匹配凭据材料
# soul 部署样例见 deploy/soul/（compose + .env.example）
```

> 本机 pnpm 若因供应链策略忽略 `esbuild/vue-demi` 构建脚本而阻断 `pnpm run`，已在 `ui/pnpm-workspace.yaml` 设置 `strictDepBuilds:false` 与 `verifyDepsBeforeRun:false`（esbuild 平台二进制经 optional 依赖 `@esbuild/linux-x64` 就位，忽略无实际影响）。

> 前端运行时三态：所有 command 调用收敛在 `ui/src/lib/api.ts` 的 `call()` 分发层——Tauri 壳内走真实 IPC；纯浏览器默认走 `ui/src/lib/web.ts` 的 REST 客户端（连真实服务端，token 仅存页面内存，刷新需重登）；仅 `VITE_USE_MOCK=1` 时切到 `ui/src/lib/mock.ts` 的内存实现（数据刷新即重置，CRUD 写操作在会话内生效）。

网关默认监听 `127.0.0.1:38440`；健康检查 `GET /health`；模型列表 `GET /v1/models`（后两者公开，POST 入口在配置 `auth_token` 后要求 Bearer）。

---

## 13. 当前实现状态

**已交付**：M1 脚手架 · M2 Provider/Model/Offer/Route CRUD · M3 非流式链路 · M4 SSE 流式 ·
M5 Lua 插件系统（Core 层 + 报文层 raw 钩子、宿主 API、沙箱配额硬化、启用门控 `MB.requires`、示例插件、Plugins 管理页）·
M6 四协议全矩阵 + Usage/Traces 可视化 · M7 无头服务端与 web 前端（crates/server + ui web 运行时 + Docker 部署）·
M8 余额&健康看板（一次性 Lua 查询脚本 + 定时调度 + 多 key 在同一卡片内逐行展示 + 卡片式看板）与 web 点击响应优化。

- 验证命令：`cargo check --locked --workspace --all-targets`、`cargo clippy --locked --workspace --all-targets`、`cargo test --locked --workspace`、`pnpm --dir ui type-check`、`pnpm --dir ui build`。无桌面系统库时可用 `cargo test --locked --workspace --exclude moonbridge-app`，但该结果不覆盖桌面端。`crates/server/tests/lifecycle.rs` 以真实 HTTP 验证双 token 鉴权、配置不泄漏凭据、加密状态重开和进程内重启排空；存储加密与余额 HTTP 安全边界由对应 crate 的回归测试覆盖。Windows DPAPI 仍需 Windows 环境验证；构建测试不等于线上部署或浏览器可视化验收。
- **推理强度传导**：`CoreRequest.reasoning.effort` 由四个上游 adapter 各自落地——Chat 用
  `reasoning_effort`、Responses 用 `reasoning.{effort,summary}`、Anthropic 默认用
  `thinking:{type:"adaptive"}` + `output_config.effort`，端点指定 `thinking_mode:"enabled"`
  时按 effort→预算表生成 `budget_tokens` 并按 `max_tokens` 夹逼；Gemini 用
  `generationConfig.thinkingConfig.{thinkingBudget,includeThoughts}`。effort→预算表共享于
  `adapters/mod.rs`。Anthropic 入站的 enabled/adaptive 均优先读取非空的
  `output_config.effort`；enabled 缺省时才从预算反推，最低档归为 `low`，adaptive
  缺省为 `high`。disabled/未知模式维持不下发 reasoning；不改变 Gemini 预算映射或
  Chat/Responses 显式 `minimal` 的透传行为。
- **trace/DTO 命名契约**：Core IR 刻意保持 snake_case（Lua 侧直接可读），面向前端的 DTO 为
  camelCase；serde 的 `rename_all` **不会**递归作用于嵌套类型，故 `TraceRecord.usage` 需自带
  `#[serde(rename_all = "camelCase")]` 的独立 `TraceUsage` 类型（有回归测试断言 snake_case 键不出现）。
- **协议矩阵**：OpenAI Responses / Anthropic / OpenAI Chat / Google GenAI(Gemini) 四协议均实现
  入口 + 上游四象限，`builtin_registry` 注册为 4×4 全矩阵；e2e 覆盖 Responses→Anthropic、
  Chat→Chat、Anthropic 入口、Chat→Gemini 等跨协议链路（含流式）。
- **推理凭据全链路**：四协议凭据形态与来源前缀见 §4。Chat 上游 reasoning 回传（连续 assistant
  消息合并 + `reasoning_content`/`reasoning` 双写）与 `chat:` 自凭据修补了「thinking 上游多轮
  tool_calls 链必 400」；Responses 入口 → Chat thinking 上游的凭据/推理回程有端到端回归
  （`e2e_reasoning_content_roundtrips_responses_to_chat`）。
- **插件沙箱硬化**：危险全局移除 / 指令计数 / 内存上限 / 执行超时 / body 降级 / http 兜底超时（见 §6），
  均有回归测试（死循环中止、内存越界报错、超大 body 降级且保留原始报文、`require`/`package`
  逃逸通道被切断、`coroutine.wrap` 死循环仍被指令配额中止、且补丁不破坏协程正常语义）。
- **trace 落盘**：网关按 `GatewayConfig.trace_dir` 将每次请求的各阶段报文快照写入
  `<trace_dir>/<session>/<model>/<created_at>-<short_id>.json`（见 §7/§8）；流式请求的
  两个响应字段由 `StreamAssembler` 聚合（见 §8）；前端 Traces 页可浏览/查看/删除。
- **前端**：Dashboard / Providers / Models(+Offer) / Routes / Plugins(在线编辑) / Usage(图表) /
  Traces(浏览) / Settings 全部打通；脚本与 JSON 编辑走 `components/ui/CodeEditor.vue`
  （CodeMirror 6 + oneDark，lua 经 `@codemirror/legacy-modes` 的 `StreamLanguage`，json 经 `@codemirror/lang-json`）。
- **钩子全接线**：`filter_content` 已挂载非流式与流式两条回程（流式连带压制被丢块的
  增量）；`MB.init` / `MB.shutdown` 由 `serve_with_shutdown` 成对扇出（见 §5）。三项均有
  带对照组的 e2e（`e2e_blocks_survive_without_filter_content` 等）——对照组断言「不挂钩子
  时块必须原样到达」，防止 e2e 因上游报文本身不含该块而假绿。
- **会话水印**：为不带会话标识的客户端（Qwen Code 等）补上 `ctx.session_id`——marker
  嵌进首个推理块的明文首部（`" [mb:<载荷>]"`，载荷即 session id），下一轮入站**先剥净
  再转发**，故 marker 永不进入上游 prompt、模型无从模仿；uuid 载荷自带身份，活跃表
  淘汰/重启不改判会话（下游 `x-opencode-session` 亲和头恒定）；外部身份仍经定长短 tag
  走活跃表（LRU）还原，淘汰时联动清理插件侧 `mb.session`（见 §8）。e2e 覆盖：多轮
  marker 稳定与会话归属、淘汰后身份还原、陈旧短 tag 合成确定性身份、
  `upstreamRequest.body` 全程无 marker 而 `clientRequest` 照实留痕、tool_use 轮照常打标、
  无推理块轮次不打标（流式与非流式各一）、关闭开关后仍剥除入站 marker、外部
  `session_id` 优先于 marker。流式侧断言逐帧解析 SSE（marker 为 thinking 首部增量、
  不占新块、signature_delta 随行、responses done item 摘要同带 marker）。
- **短路不再绕过审计**：8 个 `ShortCircuit`/`Abort` 返回点统一走 `answered` / `aborted`
  → `finish_audit`，插件代答与被插件拒绝的请求都留下 usage 行和 trace 文件（见 §8）。
- **无头服务端与 web 前端（M7，见 §10）**：`crates/server` 同端口合并网关 + `/api/*`
  管理 REST（与 src-tauri commands 1:1，40+ 端点）+ SPA 静态托管；admin 与 gateway
  token 分离，管理凭据不保存/回显，配置读取隐藏网关 token，无关保存不持久化环境覆盖。
  前端 `call()` 三分发（IPC / mock / REST），web token 仅存页面内存，刷新/401 重登，
  页面有 CSP；插件导入使用 JSON `{files:[{name,content}]}`。网关进程内优雅重启，
  状态报告实际绑定地址，每代仅一个余额调度器并取消旧任务。Docker 多阶段构建
  （`rust:1.95-bookworm` → `debian:bookworm-slim`，uid 10001），`deploy/soul/` 提供
  compose 样例；实际部署状态不在本次文档修订验证范围内。
- **入口鉴权口径**：配置 `auth_token` 后，POST 入口（`/v1/responses` / `/v1/messages` /
  `/v1/chat/completions`）一律要求 Bearer；`GET /v1/models` 与 `/health` 刻意公开
  （允许裸奔——模型目录不视为敏感，与多数 OpenAI 兼容服务一致，探活/监控可无凭据
  拉取）。e2e 钉死该口径（`e2e_models_is_public_but_post_entry_requires_bearer`）。

**可选后续增强**（非阻塞）：

- Gemini 入口挂载 HTTP 路由（当前 Gemini 主要作上游）。
