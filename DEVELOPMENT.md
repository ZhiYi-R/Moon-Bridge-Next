# Development

## 技术栈与项目结构

项目采用 Rust Workspace + Tauri 2 + Vue 3 构建。后端按职责拆分为多个 crate，保持单向依赖：

```
crates/core       Core IR（协议无关的请求/响应/流事件）与基础错误类型，无内部依赖
crates/protocol   四协议入口/上游 Adapter、PluginHooks trait、原始报文载体、协议注册表
crates/plugin     mlua 运行时、mb.* 宿主 API、沙箱配额、LuaPluginRegistry
crates/store      SQLite（rusqlite）+ 版本化迁移 + 各领域 DAO
crates/gateway    axum HTTP 服务、请求编排（dispatch）、故障转移、用量统计、请求留痕
crates/server     无头服务端：单端口托管 LLM 网关、/api/* 管理 REST 与前端静态文件
src-tauri         Tauri 桌面端：IPC commands、系统托盘、引导配置
ui                Vue 3 前端（支持 Tauri IPC、浏览器 REST、内存 Mock 三种运行模式）
```

Crate 依赖关系：`protocol → core`，`plugin → core + protocol`，`store → core`，`gateway → core + protocol + plugin + store`，`server → gateway + store + core`，`app → gateway + store + core`。

架构细节、钩子契约、数据库 Schema 与请求生命周期见 [docs/architecture.md](docs/architecture.md)。修改核心行为时请同步更新该文档。

## 环境要求

- **Rust**：`Cargo.toml` 声明 `1.85+`，锁定的依赖树（`icu_properties 2.3+`）实际要求 `rustc >= 1.88`（Docker 构建使用 `rust:1.95-bookworm`）。
- **Node.js**：Node 20+ 与 `pnpm`。
- **桌面端系统依赖**：Linux 下编译 `src-tauri` 需要 `webkit2gtk-4.1`、`librsvg2` 等依赖（详见 Tauri 官方文档）。无头服务端 `moonbridge-server` 无桌面系统库依赖。

## 常用命令

```bash
# 后端：编译与测试（全 workspace）
cargo check --workspace
cargo test  --workspace

# 无桌面系统库环境（如服务器/CI）：排除 Tauri 桌面壳
cargo test  --workspace --exclude moonbridge-app

# 前端：安装依赖与构建
pnpm --dir ui install
pnpm --dir ui build

# 前端：浏览器开发模式（默认通过 Vite 代理连接本地 127.0.0.1:38440 后端）
pnpm --dir ui dev

# 前端：纯内存 Mock 模式（无需启动后端）
VITE_USE_MOCK=1 pnpm --dir ui dev

# 启动无头服务端（本地开发示例；生产环境请使用不同的随机令牌）
cargo run -p moonbridge-server -- --admin-token dev-admin-token --gateway-token dev-gateway-token

# 启动桌面应用（开发热重载）
cargo tauri dev

# 构建桌面安装包
cargo tauri build
```

补充说明：

- **前端运行模式**：前端请求统一封装在 `ui/src/lib/api.ts` 的 `call()` 中。Tauri 环境下调用 IPC；纯浏览器环境下默认通过 `ui/src/lib/web.ts` 调用 `/api/*` REST 接口（管理令牌仅保存在页面内存，刷新需重新登录）；设置 `VITE_USE_MOCK=1` 时使用 `ui/src/lib/mock.ts` 的内存实现。
- **pnpm 构建配置**：`ui/pnpm-workspace.yaml` 已配置 `strictDepBuilds: false` 与 `verifyDepsBeforeRun: false`，避免因忽略 `esbuild` / `vue-demi` 构建脚本而阻断开发命令。
- **默认端口與鉴权**：网关默认监听 `127.0.0.1:38440`。`GET /health` 与 `GET /v1/models` 公开访问；Web 模式下 LLM POST 入口强制要求独立的网关令牌。

## Docker 与服务器部署

根目录 `Dockerfile` 采用多阶段构建（构建前端 → 编译 `moonbridge-server` → `debian:bookworm-slim` 运行时，以 `uid 10001` 运行，暴露端口 `38440`，挂载卷 `/data`）：

```bash
docker build -t moonbridge-next:local .
docker run -d -p 38440:38440 \
  -e MOONBRIDGE_ADMIN_TOKEN -e MOONBRIDGE_GATEWAY_TOKEN \
  -e MOONBRIDGE_KEY_FILE=/data/moonbridge.key \
  -v "$PWD/data:/data" --restart unless-stopped moonbridge-next:local
```

部署注意事项：

- **目录权限**：容器以 `uid 10001` 运行，挂载宿主机目录前需执行 `chown -R 10001:10001 ./data`。
- **令牌分离**：`MOONBRIDGE_ADMIN_TOKEN`（或 `--admin-token`）专用于 `/api/*` 管理接口，不落盘、不回显；`MOONBRIDGE_GATEWAY_TOKEN`（或 `--gateway-token`）用于网关推理入口，不能为空且不得与管理令牌相同。读取配置时 `authToken` 返回 `null`；保存配置时传入空值将保留既有网关令牌。
- **进程内重启**：管理界面的「重启网关」在进程内排空请求、执行插件生命周期钩子并重新监听端口，同时取消上一代配额调度任务，不退出容器进程。
- **凭据加密与备份**：数据库中的 Provider 凭据与配额配置使用 AES-GCM 加密，密钥文件默认位于 `<db>.key`（可通过 `--key-file` / `MOONBRIDGE_KEY_FILE` 指定）。备份与迁移时必须同时保留数据库与密钥文件；跨版本回滚需恢复升级前的数据库及对应密钥。
- **配额查询网络限制**：配额插件的 HTTP 请求默认拒绝私网与云元数据地址。如需放行内网配额接口，通过环境变量 `MOONBRIDGE_QUOTA_PRIVATE_ORIGINS` 配置精确 origin 的 JSON 数组（如 `["https://quota.internal.example:8443"]`）。
- **Docker Compose 示例**：见 `deploy/soul/`。

## 代码检查与测试

提交修改前请运行以下检查：

```bash
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets
cargo test --workspace
# 无桌面系统依赖时使用：
cargo test --workspace --exclude moonbridge-app
pnpm --dir ui type-check
pnpm --dir ui build
```

修改插件宿主 API 或钩子契约时，请同步更新 `plugins/moonbridge.lua`（LSP 类型定义）并运行 `crates/plugin` 下的示例插件回归测试。

## 插件调试

- **日志**：插件 `mb.log.*` 输出至 `tracing`（target 为 `plugin`），可通过 `RUST_LOG` 调整日志级别。
- **报文留痕**：配置 `trace_dir` 后，每次请求四个阶段的原始报文快照会写入 `<trace_dir>/<session>/<model>/`，可在 Traces 页面对照查看插件改写前后的报文。
