# Development

## 技术栈与结构

Rust workspace + Tauri 2 + Vue 3。后端按分层拆成多个 crate，依赖方向单向（编译期禁止反向依赖）：

```
crates/core       Core IR（协议中立的请求/响应/流事件）、错误类型 —— 无内部依赖
crates/protocol   四协议的入口/上游 adapter、PluginHooks trait、报文载体、Registry
crates/plugin     mlua 运行时、mb.* 宿主 API、沙箱配额、LuaPluginRegistry
crates/store      SQLite（rusqlite）+ 手写 migration + 各表 DAO
crates/gateway    axum server、dispatch 编排、provider 管理、usage、trace
src-tauri         Tauri 壳：commands、托盘、引导配置
ui                Vue 3 前端
```

依赖方向：`protocol → core`，`plugin → core + protocol`，`store → core`，`gateway → core + protocol + plugin + store`，`app → gateway + store + core`。

设计决策、钩子契约、存储 schema、请求生命周期这些真正的细节都在 [docs/architecture.md](docs/architecture.md)——那是二次开发的入口文档，改了行为记得同步它。

## 环境准备

- Rust 1.85+
- Node 20+ 与 pnpm
- Linux 上跑 Tauri 2 需要 webkit2gtk-4.1、librsvg2 等系统依赖（见 Tauri 官方文档）

## 常用命令

```bash
# 后端：编译 + 测试（全 workspace）
cargo check --workspace
cargo test  --workspace

# 前端：安装 + 构建
pnpm --dir ui install
pnpm --dir ui build

# 前端：纯浏览器开发（无需 Tauri 壳，自动启用内存 mock 数据）
pnpm --dir ui dev
# 浏览器访问 http://localhost:5173，header 会出现琥珀色「MOCK 数据」徽标

# 桌面应用（开发热重载）：从项目根运行
cargo tauri dev

# 打包
cargo tauri build
```

几个说明：

- **前端 mock 模式**：所有 command 调用收敛在 `ui/src/lib/api.ts` 的 `call()` 分发层，非 Tauri 环境（或 `VITE_USE_MOCK=1`）自动切到 `ui/src/lib/mock.ts` 的内存实现——数据刷新即重置，写操作在会话内生效。Tauri 壳内不受影响，仍走真实 IPC。
- **pnpm 构建脚本**：若 pnpm 因供应链策略忽略 `esbuild/vue-demi` 的构建脚本而阻断运行，`ui/pnpm-workspace.yaml` 已设 `strictDepBuilds:false` 与 `verifyDepsBeforeRun:false`。
- 网关默认监听 `127.0.0.1:38440`；健康检查 `GET /health`，模型列表 `GET /v1/models`。

## 测试基线

当前 `cargo test --workspace` 全绿（200+ 项，覆盖 core / protocol / plugin / store / gateway e2e / app），`cargo check --workspace --all-targets` 与 `cargo clippy --workspace --all-targets` 零告警，`pnpm --dir ui type-check` 无错。提交前请保持这个状态。

写插件相关改动时，注意 `plugins/moonbridge.lua`（LSP stub）要与 `crates/plugin/src/{host,runtime,convert,bridge}.rs` 保持同步；示例插件 `plugins/examples/` 有加载回归测试，改钩子语义时顺手跑一下。

## 插件调试

- 插件日志走 tracing（target=`plugin`），带插件名，调日志级别用 `RUST_LOG` 即可。
- 开 trace（`GatewayConfig.trace_dir`）后，每次请求各阶段的原始报文都会落盘到 `<trace_dir>/<session>/<model>/`，是排查插件改写是否符合预期的最快手段。
