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
src-tauri         moonbridge-app       Tauri 壳：commands(调 gateway/store) + tray + 引导配置
ui                —                    Vue3 前端
```

**依赖方向（单向，禁止反向）**：

```
core                       (无内部依赖)
protocol  → core
plugin    → core, protocol (实现 protocol 定义的 PluginHooks trait)
store     → core
gateway   → core, protocol, plugin, store
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
    ToolUse { id, name, namespace, input }, ToolResult { tool_use_id, content, is_error },
    Reasoning { text, signature },
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

`MB.init` / `MB.shutdown` 由 `PluginHooks::init_all` / `shutdown_all` 扇出，**唯一调用点**
是 `server::serve_with_shutdown`：绑定端口成功后、开始服务前跑 init（按加载顺序），服务退出后
跑 shutdown（按逆序）。放在这里而不是 `bootstrap`：两者都得 await Lua，而 `bootstrap` 是同步
函数；且绑定失败时不该留下「init 跑过、shutdown 永不跑」的不配对状态。

- 不经 capability、不经 provider 三态门控：初始化是插件自身的事，与它对哪些请求生效无关。
- 单插件 init/shutdown 抛错只记 warn，不阻断其它插件（与请求链路钩子的容错口径一致）。
- `forget_session(session_id)` 同属这一层：由会话表 FIFO 淘汰触发（见 §8），跨插件清掉该会话在
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
  - **内存上限**：`Lua::set_memory_limit`（默认 256 MB），越界分配触发 `MemoryError`。
  - **执行超时**：hook 内附带 wall-clock 截止时间（`call_timeout`），覆盖缓慢（非死循环）的长计算。
  - **body 降级**：raw body 超 `max_body_bytes` 时不展开为 Lua table（置 `nil` + `body_truncated` 标记），回写时保留原始报文，避免超大报文撑爆沙箱。
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

当前 schema 版本 **V8**（`schema.rs` 的 `MIGRATIONS` 按版本升序手写，`schema_version` 表记录已应用版本）。

| 表 | 关键列 |
|----|--------|
| `providers` | key PK, version, user_agent, web_search_json, extra_json, enabled, timestamps。**注意：`protocol` / `base_url` / `api_key_enc` 三列已分别由 V3、V4 删除**，迁至 `provider_endpoints` |
| `provider_endpoints` | provider_key + idx PK, protocol, base_url, api_key_enc —— 一个 Provider 持多端点，**每端点独立协议与独立 API Key**；按 `idx` 升序故障转移 |
| `models` | slug PK, display_name, context_window, **max_output_tokens（V8，可空；models.dev `limit.output`）**, modalities_json, reasoning_levels_json, extra_json。**注意：`pricing_json` 已由 V7 删除**，定价口径统一在 offers |
| `offers` | provider_key + model_slug PK, pricing_json, **endpoint_protocol（V6，可空）** —— 非空时该模型只走 provider 中匹配该协议的端点，为空则全端点按 idx 故障转移 |
| `routes` | alias PK, model_slug, provider_key, extra_json |
| `plugins` | name PK, source(lua), script_ref, enabled, config_json, scopes_json, capabilities_json |
| `plugin_bindings` | plugin_name + scope + scope_key PK, enabled, config_json |
| `usage_records` | id PK, session_id, model, upstream_model, input/output/cache_read/cache_write/**reasoning**_tokens, cost, status, error, latency_ms, **ttft_ms**, created_at |
| `settings` | key PK, value_json |

- api_key 加密：`EncKey` 抽象（脚手架用 `PlaintextKey`，**当前 API Key 明文落库**，生产可换 aes-gcm/keyring）。列名 `api_key_enc` 是为切换预留的。
- `status` 取值 `ok` / `error` / `aborted`（流未读尽即结束，如客户端断开）；`ttft_ms` 仅流式请求有值。
- trace 大对象存文件系统 `app_data_dir/traces/<session>/<model>/<seq>.json`，不入库。

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
             → [CORE] filter_content（丢块连带压制同 index 增量；水印在 message_stop 前插独立块）
             → [CORE] encode → [RAW] on_client_chunk_raw → 回写入口 SSE(axum Sse)
     [非流式] [RAW] on_upstream_response_raw → to_core_response → [CORE] on_response
             → [CORE] filter_content → 追加会话水印
             → from_core_response → [RAW] on_client_response_raw → 回写
  → usage 落库 + trace 落盘（若配置 trace_dir）
```

> **会话水印（`crates/gateway/src/session.rs`）**：给助手**纯文本**输出末尾附一段
> `[mb:xxxxxx]`（6 位小写 hex 短 tag），客户端下一轮把整段历史带回来时即可认回同一会话。
> 存在的理由：Qwen Code 这类客户端在请求头与请求体里都不带会话标识（其 `prompt_cache_key`
> 注入路径以 hostname 恰为 `api.openai.com` 为前提，经网关时不生效），于是 `ctx.session_id`
> 恒为 `None`，插件的 `mb.session` 与 trace 的会话目录一起失去粒度。
>
> - **入站即剥除，剥完才往下走**：`extract_from_request` 在 `to_core_request` 之后、路由与
>   插件 `on_request` 之前原地改写 `CoreRequest`。上游模型的输入里永远不出现 marker，因此
>   不存在「模型模仿 marker」的可能（模仿只在 marker 进入上下文时才会发生），marker 的唯一
>   存活期是客户端本地 transcript。trace 的 `clientRequest` 记剥除**前**的客户端原样、
>   `upstreamRequest.body` 记剥除**后**的转发内容，两者对照即是这条不变量的证据。
> - **tag ≠ session id**：短 tag 只是 `SessionTable`（`tag → uuid`，按登记顺序 FIFO 淘汰）里
>   指向完整 uuid 的键，省 token 而内部主键仍是 uuid。表深由 `session_table_depth` 配置
>   （默认 64，`<1` 钳到 1）；tag 由 id 定长哈希（FNV-1a 低 24 bit）派生——不能只挑 id 里
>   现成的 hex 字符，外部 `session_id` 未必是 uuid，挑出的短 tag 下一轮认不出来，水印就会
>   逐轮在 transcript 里累积。撞车时换新 tag 而非顶掉已有会话。
> - **解析优先级**：外部显式身份（body `session_id` / `previous_response_id` /
>   `X-Codex-Window-Id`）> marker 命中活跃表 > 新分配。tag 合法但不在表内（被淘汰 / 重启）
>   按新会话处理。
> - **两处打标**：非流式 `append_to_response` 附在最后一个文本块末尾（多隔一个空白，剥除时
>   连带吃掉，故打标→剥除字节还原）；流式 `marker_blocks` 在 `message_stop` 前插一段
>   **独立 text 块**（`start → delta → stop`，index 取已见最大 +1）——不并入已有文本块是为了
>   避开在各入口编码器的块状态机里重放事件。两处都遵守「整轮出现过 `tool_use` 就不打标」，
>   非流式还要求至少有一个文本块，绝不凭空造块。独立块被剥除后会变空块，`strip_blocks`
>   连同空块一起删掉（Anthropic 拒收空 text 块），但保底不把 `content` 删成空数组。
> - **淘汰即清理**：被 FIFO 挤出的 session id 交回 `dispatch::forget_sessions` →
>   `PluginHooks::forget_session` → `SessionStore::clear_session`，回收插件侧 `mb.session` 的桶。
> - **代价与关闭**：每轮多约 5 个 token，且客户端 transcript 里看得见这个后缀。
>   `session_marker = false` 时不再打标，但**入站 marker 照剥**（客户端可能带着开启期间
>   留下的历史，不该污染上游 prompt）。已经自带会话标识的客户端（走 `session_id` 字段或
>   `X-Codex-Window-Id`）可直接关掉。

> **流式收尾由 `StreamAudit::Drop` 承担**，而非写在读流循环之后。流可能以三种方式结束——
> 正常读尽（`ok`）、中途出错并已下发带内 error 事件（`error`）、客户端断开或上游提前关闭
> （`aborted`）；只有 Drop 能同时覆盖，尤其是客户端断开时整个生成器被直接丢弃、循环后的代码
> 根本不会执行。解码错误与**编码错误**都以 `'stream` 标签跳出整条流（编码错误原先只 break 内层
> `for`，会反复刷 error 事件）；raw chunk 钩子返回 `Err` 时上下游侧对称地记 `warn` 后放行，
> 不再被 `if let Ok(..)` 静默吞掉。

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

模块划分：`server`（axum 路由/启动，含 `serve_with_shutdown` 优雅关闭）、`dispatch`（编排 RAW/CORE 两组钩子）、`stream`（SSE 编排）、`router`（别名解析）、`session`（会话水印编解码 + 活跃会话 FIFO 表）、`upstream`（reqwest 客户端 + SSE 读取）、`bridge`（`HostBridge` 实现）、`usage`（统计落库）、`trace`（报文快照落盘）、`state`（`AppState`，含 `sessions: SessionTable`）、`config`（`GatewayConfig`）。

顶层入口：`bootstrap(config, db) -> Arc<AppState>`（装配内置 adapter、加载插件、构建 HTTP 客户端）、`serve` / `server::serve_with_shutdown`。

---

## 9. Tauri 应用层（src-tauri）

- **引导配置**：`app_config_dir/config.toml` → `AppConfig { gateway: GatewayConfig, logLevel, autoStart }`。其余业务配置全部入 SQLite。
- **状态**：`ManagedState { db, paths, config, gateway }`，网关以 tokio task + oneshot 优雅关闭信号驱动启停。
- **`gateway` 句柄锁的并发约束**：`start_gateway` / `stop_gateway` 等命令必须**避免在持有 `gateway` 互斥锁的同时 `.await`**，也不能在已持锁的路径上再取一次同把锁——`gateway_start` 被重复调用时会自锁死（前端连点即触发）。现在由 `has_live_gateway()`（短临界区，仅判断句柄存在且 `task` 未结束）先行幂等返回，`status()` 也在**一次**加锁内同时读出 running 与 addr。回归测试 `start_gateway_while_running_does_not_deadlock` 把风险调用放进**独立 OS 线程 + 独立 current-thread runtime**、用 `mpsc::recv_timeout` 断言，因为 `tokio::time::timeout` 与被阻塞的 future 同属一个任务、计时器永远得不到轮询，无法用来证死锁。
- **commands**（前端 `invoke`）：
  - 网关：`gateway_start / stop / restart / status`
  - CRUD：`provider_* / model_* / offer_* / route_* / plugin_*（含 `plugin_read_script` / `plugin_write_script` 在线编辑）/ binding_* / usage_* / settings_*`
  - 模型目录：`catalog_fetch`（后端 reqwest 拉取 `models.dev/api.json`，解析精简为扁平候选列表）/ `catalog_import`（勾选批量 upsert 到 `models` 表，含定价）
  - Trace：`trace_list / trace_read / trace_delete`（只读浏览 `trace_dir`，含路径穿越校验）
  - 应用：`app_info / config_get / config_set`
- **托盘**：显示主窗口 / 一键启停网关 / 退出；左键单击显示窗口。
- **插件**：single-instance（桌面，最先注册）、log、process、opener、dialog、window-state。
- **日志**：Rust 侧 tracing-subscriber（`EnvFilter`，默认 info）；前端 console 由 tauri-plugin-log 承接，两者互不冲突。

---

## 10. 前端（ui）

Vue 3.5 + Vite 7 + TS 5 + Pinia + Vue Router + TailwindCSS 3 + shadcn-vue 风格组件（radix-vue + cva）。

- `src/lib/api.ts`：前后端契约层（DTO 类型 + command 封装，按领域分组）。
- `src/stores/`：Pinia（gateway 状态、provider 列表）。
- `src/router`：hash 路由（Tauri 自定义协议友好）。
- `src/views/`：Dashboard（网关状态 + 用量 + Provider 概览）、Providers（完整 CRUD）、Models（模型 CRUD + 从 models.dev 搜索勾选批量导入 + provider 维度 Offer 管理，Offer 可绑定端点协议）、Routes、Plugins（在线脚本编辑 + 启停/增删 + 一键重启网关生效）、Usage（汇总卡片 + token 时序堆叠柱图 + 模型分布条图 + 明细表，纯CSS/SVG 无额外依赖）、Traces（主从布局浏览 + 各阶段报文 JSON + 删除）、Settings。
- `src/components/ui`：Button / Badge / Input / Label / Card（精简 shadcn 风格）。

---

## 11. 构建与运行

前置：Rust 1.85+、Node 20+/pnpm、Tauri 2 的 Linux 依赖（webkit2gtk-4.1、librsvg2 等）。

```bash
# 后端：编译 + 测试（全 workspace）
cargo check --workspace
cargo test  --workspace

# 前端：安装 + 构建
pnpm --dir ui install
pnpm --dir ui build

# 前端：纯浏览器开发（无需 Tauri 壳，自动启用内存 mock 数据）
pnpm --dir ui dev
# 然后浏览器访问 http://localhost:5173；header 会出现琥珀色「MOCK 数据」徽标

# 桌面应用（开发热重载）：从项目根运行
cargo tauri dev

# 打包
cargo tauri build
```

> 本机 pnpm 若因供应链策略忽略 `esbuild/vue-demi` 构建脚本而阻断 `pnpm run`，已在 `ui/pnpm-workspace.yaml` 设置 `strictDepBuilds:false` 与 `verifyDepsBeforeRun:false`（esbuild 平台二进制经 optional 依赖 `@esbuild/linux-x64` 就位，忽略无实际影响）。

> 前端 mock 模式：所有 command 调用收敛在 `ui/src/lib/api.ts` 的 `call()` 分发层，非 Tauri 环境（纯浏览器）或 `VITE_USE_MOCK=1` 时自动切换到 `ui/src/lib/mock.ts` 的内存实现（数据刷新即重置，CRUD 写操作在会话内生效）。Tauri 壳内不受影响，仍走真实 IPC。

网关默认监听 `127.0.0.1:38440`；健康检查 `GET /health`；模型列表 `GET /v1/models`。

---

## 12. 当前实现状态

**已交付**：M1 脚手架 · M2 Provider/Model/Offer/Route CRUD · M3 非流式链路 · M4 SSE 流式 ·
M5 Lua 插件系统（Core 层 + 报文层 raw 钩子、宿主 API、沙箱配额硬化、示例插件、Plugins 管理页）·
M6 四协议全矩阵 + Usage/Traces 可视化。

- 5 个 lib crate + src-tauri + ui 全部编译通过；`cargo test --workspace` 全绿 **152 项**
  （core 9 / protocol 41 / plugin 19 lib + 4 integration / store 5 / gateway 45 lib + 23 e2e / app 6）；
  `cargo check --workspace --all-targets` 与 `cargo clippy --workspace --all-targets` 均**零告警**；
  `pnpm --dir ui type-check`（`vue-tsc --noEmit`）无错。
  原存量的 6 条 clippy 提示已全部清理：4 处真改（两处 markdown 文档列表缺空行分隔、
  `convert.rs` 双层 `if let` 收敛为 `.ok().flatten()`、`commands/trace.rs` 的
  `sort_by` 改 `sort_by_key(Reverse(..))`）；`protocol/adapter.rs` 的 2 处
  `wrong_self_convention` 用**带理由的作用域 `#[allow]`** 保留——`from_core_*`/`to_core_*`
  表达的是 Core ↔ 协议的转换方向且与配对方法对称，不是构造函数，而 `&self` 为
  `Arc<dyn Adapter>` 动态派发所必需，改名只会破坏对称性。
- **推理强度传导**：`CoreRequest.reasoning.effort` 由四个上游 adapter 各自落地——Chat 用
  `reasoning_effort`、Responses 用 `reasoning.{effort,summary}`、Anthropic 用
  `thinking:{type,budget_tokens}`（effort→预算表，并按 `max_tokens` 夹逼）、Gemini 用
  `generationConfig.thinkingConfig.{thinkingBudget,includeThoughts}`。effort→预算表共享于
  `adapters/mod.rs`。
- **trace/DTO 命名契约**：Core IR 刻意保持 snake_case（Lua 侧直接可读），面向前端的 DTO 为
  camelCase；serde 的 `rename_all` **不会**递归作用于嵌套类型，故 `TraceRecord.usage` 需自带
  `#[serde(rename_all = "camelCase")]` 的独立 `TraceUsage` 类型（有回归测试断言 snake_case 键不出现）。
- **协议矩阵**：OpenAI Responses / Anthropic / OpenAI Chat / Google GenAI(Gemini) 四协议均实现
  入口 + 上游四象限，`builtin_registry` 注册为 4×4 全矩阵；e2e 覆盖 Responses→Anthropic、
  Chat→Chat、Anthropic 入口、Chat→Gemini 等跨协议链路（含流式）。
- **插件沙箱硬化**：危险全局移除 / 指令计数 / 内存上限 / 执行超时 / body 降级 / http 兜底超时（见 §6），
  均有回归测试（死循环中止、内存越界报错、超大 body 降级且保留原始报文、`require`/`package`
  逃逸通道被切断、`coroutine.wrap` 死循环仍被指令配额中止、且补丁不破坏协程正常语义）。
- **trace 落盘**：网关按 `GatewayConfig.trace_dir` 将每次请求的各阶段报文快照写入
  `<trace_dir>/<session>/<model>/<created_at>-<id>.json`（见 §7/§8）；前端 Traces 页可浏览/查看/删除。
- **前端**：Dashboard / Providers / Models(+Offer) / Routes / Plugins(在线编辑) / Usage(图表) /
  Traces(浏览) / Settings 全部打通；脚本与 JSON 编辑走 `components/ui/CodeEditor.vue`
  （CodeMirror 6 + oneDark，lua 经 `@codemirror/legacy-modes` 的 `StreamLanguage`，json 经 `@codemirror/lang-json`）。
- **钩子全接线**：`filter_content` 已挂载非流式与流式两条回程（流式连带压制被丢块的
  增量）；`MB.init` / `MB.shutdown` 由 `serve_with_shutdown` 成对扇出（见 §5）。三项均有
  带对照组的 e2e（`e2e_blocks_survive_without_filter_content` 等）——对照组断言「不挂钩子
  时块必须原样到达」，防止 e2e 因上游报文本身不含该块而假绿。
- **会话水印**：为不带会话标识的客户端（Qwen Code 等）补上 `ctx.session_id`——纯文本输出
  尾随 `[mb:xxxxxx]` 短 tag，下一轮入站**先剥净再转发**，故 marker 永不进入上游 prompt、
  模型无从模仿；活跃会话由可配深度的 FIFO 表承载，淘汰时联动清理插件侧 `mb.session`（见 §8）。
  6 项 e2e 覆盖：多轮 tag 稳定与会话归属、陈旧 tag 改派新会话、`upstreamRequest.body`
  全程无 marker 而 `clientRequest` 照实留痕、工具轮不打标（流式与非流式各一）、
  关闭开关后仍剥除入站 marker、外部 `session_id` 优先于 marker。流式侧断言逐帧解析
  SSE（块 index 连续、`content_block_start`/`stop` 配对、水印不成为末帧）。
- **短路不再绕过审计**：8 个 `ShortCircuit`/`Abort` 返回点统一走 `answered` / `aborted`
  → `finish_audit`，插件代答与被插件拒绝的请求都留下 usage 行和 trace 文件（见 §8）。

**可选后续增强**（非阻塞）：

- usage `cost` 按 offers pricing 实际计价（当前恒为 0）。
- Gemini 入口挂载 HTTP 路由（当前 Gemini 主要作上游）。
