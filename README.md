# Moon Bridge Next

[English](README.en.md) · 中文

Moon Bridge Next 是一个本地 LLM 网关，把客户端和上游模型服务之间互不兼容的 API 协议打通：客户端用它熟悉的协议把请求发给网关，网关翻成上游服务商的协议再转发出去。OpenAI Responses、OpenAI Chat Completions、Anthropic Messages、Google Gemini 四种协议任意组合均可互转。

协议翻译之外，网关还内置 Lua 插件系统——插件既能修改协议无关的请求/响应语义，也能直接改写进出的原始 HTTP 报文；另有用量计量、请求留痕（Traces）、多端点故障转移等配套能力。所有配置都在自带的桌面应用里完成，不需要手工维护配置文件。

## 协议支持

协议互转以「中间表示居中」实现：入口请求先归一化为内部统一的 Core IR（`CoreRequest` / `CoreResponse` / `CoreStreamEvent`），再由上游适配器翻成目标协议。这样 N×M 的成对转换就降为 N+M——新增一种协议只需实现它与 Core IR 之间的双向适配。

| 协议 | 入口（客户端 → 网关） | 上游（网关 → 服务商） |
|------|:---:|:---:|
| OpenAI Responses（`/v1/responses`） | ✓ | ✓ |
| OpenAI Chat Completions（`/v1/chat/completions`） | ✓ | ✓ |
| Anthropic Messages（`/v1/messages`） | ✓ | ✓ |
| Google Generative AI / Gemini | 已实现，暂未挂载路由 | ✓ |

四种协议的双向适配均覆盖非流式与流式（SSE）。Core IR 统一约定了内容块类型（文本、图像、工具调用、工具结果、推理）与用量口径（输入 token 含缓存读写、输出 token 含推理），跨协议转换不会丢失这些信息。

另有 `GET /health`（健康检查）与 `GET /v1/models`（模型列表）两个辅助接口。

## 功能概览

### Provider 与模型管理

- **多端点 Provider**：同一 Provider 可配置多个端点，各有独立的协议、base URL 与 API Key，请求按端点顺序自动故障转移。
- **模型目录**：可从 models.dev 检索并批量导入模型，上下文窗口、最大输出 token、模态、推理档位与定价一并带入；也支持完全手工维护。
- **Offer 与定价**：模型经由 Offer 挂到 Provider 上。Offer 携带价目（USD/1M tokens，区分输入、输出、缓存读、缓存写、推理五类单价，支持长上下文分档计价），并可绑定特定端点协议，控制同一模型在多协议 Provider 上的路由。
- **路由别名**：客户端以别名请求模型，别名解析为「Provider + 上游模型」，更换上游或调整故障转移策略对客户端透明。

### 用量计量

每次请求记录一条用量：会话、客户端模型、上游模型、Provider、五类 token 计数（输入 / 输出 / 缓存读 / 缓存写 / 推理）、首 token 延迟（TTFT）、总延迟、状态（成功 / 错误 / 中断）与费用。费用按记录时命中的价目现算，价目变动不回改历史记录。用量页提供汇总卡片、token 时序堆叠图、模型分布与逐条明细。

### 请求留痕（Traces）

开启 trace 后，每次请求在入站、出站、上游响应、客户端响应四个阶段的原始报文都会快照存盘；流式请求的响应由网关从事件流聚合还原为完整消息体后再记录，因此能精确区分「上游实际发送的内容」与「客户端实际收到的内容」。Traces 页支持浏览、对照查看与删除，是核对插件改写效果、排查上游问题的主要手段。

### 余额&健康看板

「余额」页按提供商分组、以卡片展示各上游 key 的配额与健康状态：每张卡片**引用一个已配置的上游服务**加一个**可选的查询 URL**；Provider 端点里的 API key 去重后，引擎对**每个 key 各执行一次脚本并自动拆成多张卡片**（逐 key 显示掩码 key 标签、独立状态与配额、可单独刷新某个 key），脚本始终只按单个 key 编写（`ctx.key`）。新建卡片可从**内置模板库**一键填充脚本——模板按各服务真实接口编写（new-api 系中转站、DeepSeek、Moonshot/Kimi 开放平台、SiliconFlow、OpenRouter、智谱 GLM Coding Plan、Kimi Coding Plan、Claude Code 订阅等），也可完全手写自定义 Lua（沙箱内执行，可用 `mb.http.request` 访问上游配额接口，返回 JSON 契约——配额百分比或「消耗/余额」金额（`unit` + `used_amount`/`left_amount`）、重置时间、摘要等；编辑器内置编写指南与**测试拉取**预览——保存前即可试跑验证脚本），支持按卡片设置定时查询间隔（可禁用）、一键全部刷新、卡片级百分比/金额显示切换；某个 key 查询失败时对应卡片标记异常并保留该 key 最近一次成功结果。配额条数量与名称完全由脚本决定。

### 会话识别

会话身份按以下优先级解析：请求显式携带的标识（body 中的 `session_id`、`previous_response_id`，或 `X-Codex-Window-Id` 请求头）优先。对完全不带会话标识的客户端（如 Qwen Code），网关会把 `[mb:<载荷>]` 标记嵌进助手**推理（CoT）块明文的首部**，客户端下一轮带回完整对话历史时即可认回同一会话；网关自分配会话的载荷就是会话 uuid，因此身份不随活跃会话表淘汰或网关重启而漂移（无推理块的轮次不打标）。标记在转发上游前被完整剥除，不进入模型上下文，也不存在被模型模仿的风险；该功能可在设置中关闭，关闭后入站方向的存量标记仍会被剥除。会话识别决定了插件 `mb.session` 的状态隔离粒度与 trace 的会话目录归类。

### 访问控制

桌面网关可为自身入口配置访问令牌（`auth_token`），配置后客户端的 POST 请求须携带 `Authorization: Bearer <token>`。`GET /health` 与 `GET /v1/models` 继续保持公开，这是既有行为。桌面模式未配置令牌时不做入口校验，仅适用于回环监听；Web 模式必须配置独立的管理令牌和网关令牌。

### 桌面应用

所有管理工作在图形界面完成：Dashboard（网关状态、用量与 Provider 概览）、Providers（端点与密钥）、Models（目录导入与 Offer）、Routes（别名）、Plugins（在线脚本编辑、启停，重启网关生效）、Usage（图表与明细）、Balance（余额&健康看板）、Traces（报文浏览）、Settings（网关参数）。系统托盘提供主窗口唤出、网关一键启停与退出。

### 服务器部署（Web 模式）

不带桌面的服务器场景可用无头服务端 `moonbridge-server`：同一端口同时提供 LLM 网关、`/api/*` 管理 REST API 与前端页面的静态托管——浏览器打开地址、填入 admin token 即可使用与桌面端相同的管理界面。

- 管理令牌由 `--admin-token` / `MOONBRIDGE_ADMIN_TOKEN` 提供，仅用于管理 API，不保存、不回显。网关令牌由 `--gateway-token` / `MOONBRIDGE_GATEWAY_TOKEN` 或既有配置提供，不能为空，也不能与管理令牌相同。
- 从旧版共享令牌升级时，将旧值放入 `MOONBRIDGE_GATEWAY_TOKEN`，另生成不同的管理令牌；模型客户端无需改 key，浏览器改用新管理令牌登录。Web 登录令牌仅保存在页面内存，刷新需重新登录；页面提供 CSP 防护。
- 管理 API 读取配置时 `authToken` 为 `null`；保存配置传 `null` 或空值保留既有网关令牌，只有显式更新才持久化新网关令牌。无关配置保存不会将环境变量覆盖值写入配置。
- 提供多阶段 `Dockerfile`（构建前端 + 编译服务端 + slim 运行时，uid 10001），`deploy/soul/` 有 docker compose 部署样例。「重启网关」在进程内优雅排空请求、执行生命周期钩子后重新监听，不依赖 supervisor；状态显示实际绑定地址。每代网关只有一个余额调度器，重启取消旧任务。
- 公网暴露时建议前置反向代理（TLS 终止），并分别保管两种令牌。

### 凭据存储与升级备份

默认文件数据库对 Provider 与余额查询凭据统一使用 AES-GCM 加密，V13 在事务中迁移旧数据与加密元数据。密钥文件默认位于 `<db>.key`，服务端可用 `--key-file` / `MOONBRIDGE_KEY_FILE` 指定。Unix 文件权限为 `0600`；Windows 使用当前用户 DPAPI 包装密钥，恢复受该账户绑定限制。已有加密数据缺少密钥或密钥错误时拒绝打开，不回退为明文。

备份必须同时保留数据库和密钥文件。升级前另存数据库备份：旧镜像不能直接回滚使用新加密数据库，回退须恢复升级前数据库及其匹配的凭据材料。前端在编辑或查询时仍可能处理凭据，加密保护的是上述字段的静态存储，并非整个数据库、配置或 trace。

### 余额查询安全边界

余额脚本的 HTTP 请求仅可访问同源或明确授权的 origin，默认拒绝私网与云元数据地址。确需访问私网时，在启动环境设置 `MOONBRIDGE_BALANCE_PRIVATE_ORIGINS` 为精确 origin 的 JSON 数组（例如 `["https://balance.internal.example:8443"]`），不能通过卡片 `extra` 自行放行；元数据地址始终禁止。请求不使用系统代理、不跟随重定向，并钉住已校验的 DNS 地址；响应解压后最多 1 MiB，单次 HTTP 最长 30 秒、整张卡片最长 45 秒。

显式配置 `egressProxy` 时，余额 HTTP 会明确报不支持代理，不会静默直连；网关推理请求仍支持代理。脚本返回 `nil` 或非法 `status` 均视为错误。new-api 模板按当前 `data.quota / 500000` 计算美元余额，不使用 `used_quota` 代替余额；Kimi 模板允许仅一个配额窗口可用。

## 使用方式

1. 启动应用，网关默认监听 `127.0.0.1:38440`（可在 Settings 中修改）。
2. 在 Providers 页新增上游：选择协议，填写 base URL 与 API Key；需要高可用时可追加多个端点。
3. 在 Models 页从 models.dev 目录导入模型（或手工新建），并为目标 Provider 创建 Offer；价目可留空，缺失时费用按 0 记录。
4. 在 Routes 页创建别名，绑定「Provider + 模型」。
5. 将客户端的 API 地址指向网关，模型名填别名：
   - OpenAI 兼容客户端：base URL 设为 `http://127.0.0.1:38440/v1`；
   - Anthropic 协议客户端：base URL 设为 `http://127.0.0.1:38440`；
   - API Key 一栏：网关配置了 `auth_token` 则填该值，否则填任意非空占位值。
6. 在 Plugins 页按需装配插件，保存后重启网关生效。

## 插件系统

插件是一个 Lua 5.4 脚本，挂在请求链路上介入流量处理。网关提供两层钩子：

- **语义层（`core`）**：触发时协议转换已完成，操作对象是统一的 Core IR——写一次，对全部四种协议生效；
- **报文层（`raw_request` / `raw_response` / `raw_stream`）**：直接读写真实 HTTP 报文——headers、body、乃至流式传输的每一个 SSE chunk，位于协议转换最外层。

### 插件能做什么

语义层的典型用法：

- 注入或改写 system 提示、追加消息、调整请求参数（`temperature`、`max_tokens`、推理强度等）；
- 通过 `inject_tools` 追加工具定义，让不带工具的客户端也能用上工具；
- 用 `filter_content` 过滤内容块（例如剥掉推理过程只留正文），或丢弃指定的流式事件；
- 统一改写错误消息；记录请求日志、按会话计数（`mb.session`）。

报文层的典型用法：

- 增删改请求/响应头：补上游要求的私有头、覆写 User-Agent、配合 `mb.crypto` 做 HMAC 签名；
- 改写出站 body：注入 metadata、给 `max_tokens` 收敛上限等；
- 做入口鉴权：缺 `Authorization` 头直接 `abort` 拒绝；
- 短路本地应答：命中缓存时直接返回响应，完全不访问上游；
- 丢弃心跳等无意义的 SSE chunk；
- 配合 `mb.provider.invoke` 做跨 Provider 编排——例如在插件里先用便宜模型给请求分类，再决定主请求的去向。

### 插件清单

脚本执行后暴露全局 `MB` 表，同时承载清单与钩子函数：

| 字段 | 说明 |
|------|------|
| `name` | 插件唯一名，以在 Plugins 页登记的名称为准 |
| `version` | 版本号 |
| `scopes` | 允许挂载的范围：`global` / `provider` / `model` / `route` |
| `capabilities` | 启用的钩子层：`core` / `raw_request` / `raw_response` / `raw_stream` |
| `config_schema` | 配置的 JSON Schema（可选），Plugins 页据此自动渲染配置表单 |
| `requires` | 启用前提（可选），如 `{ sessionMarker = true }`；前提不满足时网关拒绝启用 |
| `init` / `shutdown` | 生命周期函数（可选），分别在网关启动完成前、退出后按加载顺序/逆序调用 |

宿主提供的 API 挂在全局 `mb`（小写）下。

### 语义层钩子

| 钩子 | 说明 |
|------|------|
| `on_request(ctx, req)` | 修改请求。就地改 `req` 即生效，也可 return 一个新 table |
| `inject_tools(ctx)` | 返回要追加的工具数组 |
| `on_response(ctx, resp)` | 修改非流式响应 |
| `on_stream_event(ctx, ev)` | 处理流式事件，返回 `true` 丢弃该事件 |
| `filter_content(ctx, block)` | 返回 `true` 跳过该内容块；流式下被丢块的后续增量一并压制 |
| `transform_error(ctx, msg)` | 返回改写后的错误消息 |

`req`（CoreRequest）的主要字段：`model`（路由后的上游模型名）、`model_alias`、`system`（ContentBlock 数组）、`messages`、`tools`、`tool_choice`、`max_tokens`、`temperature`、`top_p`、`stop`、`stream`、`reasoning`、`meta`（请求级元数据：`session_id`、原始 headers、客户端标识等）。内容块形如 `{ type = "text", text = "..." }`。`resp`（CoreResponse）含 `content`、`stop_reason` 与 `usage`（`input_tokens` / `output_tokens` / `cache_read_tokens` / `cache_write_tokens` / `reasoning_tokens`）。

### 报文层钩子

| 钩子 | 所需能力 | 触发时机 |
|------|---------|---------|
| `on_client_request_raw(ctx, msg)` | `raw_request` | 入站请求（客户端 → 网关） |
| `on_upstream_request_raw(ctx, msg)` | `raw_request` | 出站请求（网关 → 上游） |
| `on_upstream_response_raw(ctx, msg)` | `raw_response` | 上游响应（非流式） |
| `on_client_response_raw(ctx, msg)` | `raw_response` | 出站响应（网关 → 客户端，非流式） |
| `on_upstream_chunk_raw(ctx, chunk)` | `raw_stream` | 上游 SSE chunk（流式） |
| `on_client_chunk_raw(ctx, chunk)` | `raw_stream` | 回写客户端的 SSE chunk（流式） |

`msg` 的字段：`stage`、`protocol`、`provider`、`method`、`url`、`status`、`headers`（保序数组 `{{k, v}, ...}`）、`body`（JSON table / 字符串 / nil；超过阈值时为 nil 且 `body_truncated = true`，原始报文照常转发）。`chunk` 的字段：`stage`、`protocol`、`provider`、`event`、`data`、`raw`。就地修改即生效；出站请求的 `method` / `url` / `headers` / `body` 四项改写全部生效。

返回值约定：返回 `nil` 放行；报文钩子可返回 `{ action = "short_circuit", status?, headers?, body? }`（本地直接应答，跳过上游）或 `{ action = "abort", message? }`（拒绝请求）；chunk 钩子可返回 `{ action = "drop" }`。短路与中止的请求同样留下用量记录与 trace，便于审计。

能力声明同时是性能开关：未声明 `raw_stream` 的插件在流式请求中完全不产生 Lua 调用，逐 chunk 转发零开销。

### 上下文与宿主 API

每次钩子调用注入只读上下文 `ctx`：`request_id`、`session_id`（无会话时为 nil）、`model_alias`、`client_protocol`（`openai-response` / `openai-chat` / `anthropic` / `google-genai`）、`upstream_protocol`、`provider`、`stream`。

全局 `mb` 提供以下宿主能力（`http` 与 `provider.invoke` 为异步函数）：

| API | 说明 |
|-----|------|
| `mb.log.debug / info / warn / error(msg)` | 结构化日志，带插件名 |
| `mb.config` | 本插件的配置，配合 `config_schema` 使用 |
| `mb.session.get(k)` / `mb.session.set(k, v)` | 会话级状态，按「插件名 + 会话」隔离；会话被淘汰时自动清理 |
| `mb.http.request({ method, url, headers, body, timeout_ms })` | HTTP 子请求，返回 `{ status, headers, body }`；受宿主 egress 代理与超时管控，未指定超时时兜底 30 秒 |
| `mb.provider.invoke(provider, model, req)` | 以 Core IR 直接调用另一个 Provider，返回 CoreResponse；跨模型编排的入口 |
| `mb.headers.get / set / remove(headers, name)` | 报文头操作，大小写不敏感、保序 |
| `mb.crypto.sha256 / hmac_sha256 / base64_encode / base64_decode` | 摘要与编码，用于上游签名等场景 |

### 一个完整的例子

下面这个插件同时声明了 `core` 与 `raw_request` 两层能力：语义层注入 system 提示并按会话计数，报文层给出站请求补头、收敛 `max_tokens` 上限：

```lua
MB = {
  version = "1.0.0",
  scopes = { "global", "provider" },
  capabilities = { "core", "raw_request" },
  config_schema = {
    type = "object",
    properties = {
      prefix     = { type = "string", title = "注入到 system 的提示语" },
      max_tokens = { type = "number", title = "max_tokens 上限" },
    },
  },
}

function MB.on_request(ctx, req)
  local n = (mb.session.get("count") or 0) + 1
  mb.session.set("count", n)
  mb.log.info(string.format("[%s] 会话 %s 第 %d 次请求",
    ctx.model_alias, ctx.session_id or "anon", n))
  table.insert(req.system, 1,
    { type = "text", text = mb.config.prefix or "回答尽量简短。" })
end

function MB.on_upstream_request_raw(ctx, msg)
  msg.headers = mb.headers.set(msg.headers, "x-moonbridge-session", ctx.session_id or "anon")
  if type(msg.body) == "table" then
    local cap = mb.config.max_tokens or 8192
    if msg.body.max_tokens and msg.body.max_tokens > cap then
      msg.body.max_tokens = cap
    end
  end
end
```

### 加载与管理

- 在 Plugins 页新建插件：直接粘贴内联脚本，或指向插件目录下的 `.lua` 文件（路径不允许穿越出插件目录）；
- 支持在线编辑脚本、启停与删除；保存改动后重启网关生效；
- 通过绑定（binding）把插件挂到声明过的 scope 上：全局、某个 Provider、某个模型或某条路由，同一插件在不同绑定下可使用不同配置；
- 单个插件的钩子抛错只记录警告并跳过该插件，不影响请求链路与其它插件。

### 沙箱与配额

每个插件独占一个 Lua 虚拟机，运行在沙箱中：

- `os`、`io`、`loadfile`、`dofile`、`require`、`package` 六个危险全局整体不可用；
- 指令数配额（默认 2 亿条）经调试钩子强制执行，覆盖插件自建的协程，死循环会被中止；
- 内存上限（默认 1024 MB）与 wall-clock 执行超时兜底缓慢型失控；
- 超大报体（默认阈值 100 MB）不展开为 Lua table，标记 `body_truncated` 后照常转发原始报文。

### 编写插件的参考材料

- `plugins/examples/log_request.lua`——语义层示例：请求日志、会话计数、system 注入、错误改写；
- `plugins/examples/raw_rewrite.lua`——报文层示例：鉴权检查、出站头改写、body 补丁、心跳 chunk 丢弃；
- `plugins/utils/FxxkDax.lua`——生产在用的最小插件：为出站请求注入上游要求的会话标识头；
- `plugins/moonbridge.lua`——LSP stub：把它加入 lua-language-server 的工作区库即可获得 `MB` 与 `mb` 的补全和类型提示。以 VS Code 为例，在 settings.json 中配置：`"Lua.workspace.library": { "/path/to/moon-bridge-next/plugins": true }`。

## 文档

- [docs/architecture.md](docs/architecture.md)：架构契约文档——Core IR 定义、钩子语义、存储 schema、请求生命周期等全部设计细节，二次开发的入口。
- [DEVELOPMENT.md](DEVELOPMENT.md)：构建、运行、测试与工程约定。

## 许可

本项目以 GPL-3.0-or-later 许可发布，许可全文见 [LICENSE](LICENSE)。
