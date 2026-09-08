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
pub struct Usage { input_tokens, output_tokens, cache_read_tokens, cache_write_tokens }

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
| `filter_content(ctx, &mut block) -> bool` | 返回 true 跳过该内容块 |
| `transform_error(ctx, msg) -> String` | 转换错误消息 |

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

- **沙箱与配额**（`crates/plugin/src/quota.rs` + `host::sandbox`）：移除 `os / io / loadfile / dofile / package.loadlib`；并施加四项硬性配额：
  - **指令计数**：每插件一个 `ExecutionBudget`，经 Lua debug hook（`every_nth_instruction`）累加，超 `max_instructions`（默认 2 亿）即中止，掐断死循环。因 `call_async` 在**协程**执行而 hook 按线程生效，运行时以 `create_thread + Thread::set_hook + into_async` 把 hook 绑定到协程，确保异步调用同样受约束。
  - **内存上限**：`Lua::set_memory_limit`（默认 256 MB），越界分配触发 `MemoryError`。
  - **执行超时**：hook 内附带 wall-clock 截止时间（`call_timeout`），覆盖缓慢（非死循环）的长计算。
  - **body 降级**：raw body 超 `max_body_bytes` 时不展开为 Lua table（置 `nil` + `body_truncated` 标记），回写时保留原始报文，避免超大报文撑爆沙箱。
  - `mb.http.request` 未显式指定超时则由 gateway bridge 施加兜底超时（默认 30s），防止 raw 钩子内挂死。
  - 配额上限由 `SandboxLimits` 描述，gateway 从 `GatewayConfig`（`max_body_bytes` / `request_timeout_secs`）派生注入。
- **HostBridge**（crates/plugin/src/bridge.rs）：受控宿主能力契约，由 gateway 实现并注入，维持 plugin 不依赖 gateway 的单向依赖。
- **示例插件**：`plugins/examples/log_request.lua`（Core 层）、`plugins/examples/raw_rewrite.lua`（报文层）。二者均有加载执行回归测试（`crates/plugin` 的 `loads_repo_example_plugins`）。

---

## 7. 存储层（crates/store）

SQLite（rusqlite, bundled + WAL），手写版本化 migration，每表一个 DAO 模块，统一由 `Database` 暴露。并发模型：单连接 + `parking_lot::Mutex` 串行化（本地网关低并发足够）。

| 表 | 关键列 |
|----|--------|
| `providers` | key PK, protocol, base_url, api_key_enc, version, user_agent, web_search_json, extra_json, enabled, timestamps |
| `models` | slug PK, display_name, context_window, modalities_json, reasoning_levels_json, pricing_json, extra_json |
| `offers` | provider_key + model_slug PK, pricing_json |
| `routes` | alias PK, model_slug, provider_key, extra_json |
| `plugins` | name PK, source(lua), script_ref, enabled, config_json, scopes_json, capabilities_json |
| `plugin_bindings` | plugin_name + scope + scope_key PK, enabled, config_json |
| `usage_records` | id PK, session_id, model, upstream_model, tokens…, cost, status, error, latency_ms, created_at |
| `settings` | key PK, value_json |

- api_key 加密：`EncKey` 抽象（脚手架用 `PlaintextKey`，生产可换 aes-gcm/keyring）。
- trace 大对象存文件系统 `app_data_dir/traces/<session>/<model>/<seq>.json`，不入库。

---

## 8. 网关请求生命周期（crates/gateway）

```
Client → axum: POST /v1/responses | /v1/messages | /v1/chat/completions
  → 认证(Bearer) / session 解析(session_id / previous_response_id / X-Codex-Window-Id)
  → [RAW] on_client_request_raw            改 headers/body，可 short_circuit / abort
  → 按入口路径选 ClientAdapter → to_core_request(raw) → CoreRequest
  → 路由解析: alias → (provider, 上游 model) → Provider + 上游 Protocol
  → [CORE] on_request / inject_tools
  → 选 ProviderAdapter → from_core_request → UpstreamRequest(headers+body)
  → [RAW] on_upstream_request_raw          改上游 headers/body/url
  → reqwest 发送(受 egress proxy)
  → [流式] 逐 chunk:
             [RAW] on_upstream_chunk_raw → [CORE] decode + on_stream_event
             → [CORE] encode → [RAW] on_client_chunk_raw → 回写入口 SSE(axum Sse)
     [非流式] [RAW] on_upstream_response_raw → to_core_response → [CORE] on_response
             → from_core_response → [RAW] on_client_response_raw → 回写
  → usage 落库 + trace 落盘（若配置 trace_dir）
```

模块划分：`server`（axum 路由/启动，含 `serve_with_shutdown` 优雅关闭）、`dispatch`（编排 RAW/CORE 两组钩子）、`stream`（SSE 编排）、`router`（别名解析）、`upstream`（reqwest 客户端 + SSE 读取）、`bridge`（`HostBridge` 实现）、`usage`（统计落库）、`trace`（报文快照落盘）、`state`（`AppState`）、`config`（`GatewayConfig`）。

顶层入口：`bootstrap(config, db) -> Arc<AppState>`（装配内置 adapter、加载插件、构建 HTTP 客户端）、`serve` / `server::serve_with_shutdown`。

---

## 9. Tauri 应用层（src-tauri）

- **引导配置**：`app_config_dir/config.toml` → `AppConfig { gateway: GatewayConfig, logLevel, autoStart }`。其余业务配置全部入 SQLite。
- **状态**：`ManagedState { db, paths, config, gateway }`，网关以 tokio task + oneshot 优雅关闭信号驱动启停。
- **commands**（前端 `invoke`）：
  - 网关：`gateway_start / stop / restart / status`
  - CRUD：`provider_* / model_* / offer_* / route_* / plugin_*（含 `plugin_read_script` / `plugin_write_script` 在线编辑）/ binding_* / usage_* / settings_*`
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
- `src/views/`：Dashboard（网关状态 + 用量 + Provider 概览）、Providers（完整 CRUD）、Models（模型 CRUD + provider 维度 Offer 管理）、Routes、Plugins（在线脚本编辑 + 启停/增删 + 一键重启网关生效）、Usage（汇总卡片 + token 时序堆叠柱图 + 模型分布条图 + 明细表，纯CSS/SVG 无额外依赖）、Traces（主从布局浏览 + 各阶段报文 JSON + 删除）、Settings。
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

- 5 个 lib crate + src-tauri + ui 全部编译通过；`cargo test --workspace` 全绿（core 9 / gateway 6+e2e 7 /
  plugin 13 / protocol 35 / store 5）；`pnpm build` 产出 dist；`vue-tsc` 类型检查无错。
- **协议矩阵**：OpenAI Responses / Anthropic / OpenAI Chat / Google GenAI(Gemini) 四协议均实现
  入口 + 上游四象限，`builtin_registry` 注册为 4×4 全矩阵；e2e 覆盖 Responses→Anthropic、
  Chat→Chat、Anthropic 入口、Chat→Gemini 等跨协议链路（含流式）。
- **插件沙箱硬化**：指令计数 / 内存上限 / 执行超时 / body 降级 / http 兜底超时（见 §6），
  均有回归测试（死循环中止、内存越界报错、超大 body 降级且保留原始报文）。
- **trace 落盘**：网关按 `GatewayConfig.trace_dir` 将每次请求的各阶段报文快照写入
  `<trace_dir>/<session>/<model>/<created_at>-<id>.json`（见 §7/§8）；前端 Traces 页可浏览/查看/删除。
- **前端**：Dashboard / Providers / Models(+Offer) / Routes / Plugins(在线编辑) / Usage(图表) /
  Traces(浏览) / Settings 全部打通。

**可选后续增强**（非阻塞）：

- 插件脚本编辑器升级为 CodeMirror（当前为等宽 textarea，无额外依赖）。
- usage `cost` 按 offers pricing 实际计价（当前恒为 0）。
- Gemini 入口挂载 HTTP 路由（当前 Gemini 主要作上游）。
