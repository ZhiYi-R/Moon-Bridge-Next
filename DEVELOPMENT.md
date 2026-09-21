# Development

## 技术栈与结构

Rust workspace + Tauri 2 + Vue 3。后端按分层拆成多个 crate，依赖方向单向（编译期禁止反向依赖）：

```
crates/core       Core IR（协议中立的请求/响应/流事件）、错误类型 —— 无内部依赖
crates/protocol   四协议的入口/上游 adapter、PluginHooks trait、报文载体、Registry
crates/plugin     mlua 运行时、mb.* 宿主 API、沙箱配额、LuaPluginRegistry
crates/store      SQLite（rusqlite）+ 手写 migration + 各表 DAO
crates/gateway    axum server、dispatch 编排、provider 管理、usage、trace
crates/server     无头服务端：同端口合并 LLM 网关 + /api 管理 REST + SPA 静态托管
src-tauri         Tauri 壳：commands、托盘、引导配置
ui                Vue 3 前端（Tauri IPC / 纯浏览器 REST 双运行时）
```

依赖方向：`protocol → core`，`plugin → core + protocol`，`store → core`，`gateway → core + protocol + plugin + store`，`server → gateway + store + core`，`app → gateway + store + core`。

设计决策、钩子契约、存储 schema、请求生命周期这些真正的细节都在 [docs/architecture.md](docs/architecture.md)——那是二次开发的入口文档，改了行为记得同步它。

## 环境准备

- Rust 1.85+（声明值；注意锁定的依赖树实际要求 ≥1.88——`icu_properties` 2.3 起要 rustc≥1.88，Docker 构建固定 `rust:1.95-bookworm`）
- Node 20+ 与 pnpm
- Linux 上跑 Tauri 2 需要 webkit2gtk-4.1、librsvg2 等系统依赖（见 Tauri 官方文档；仅桌面端需要，`moonbridge-server` 无任何系统依赖）

## 常用命令

```bash
# 后端：编译 + 测试（全 workspace）
cargo check --workspace
cargo test  --workspace

# 无桌面系统库的环境（如 CI/服务器）：排除 Tauri 壳，结果不代表桌面端验证
cargo test  --workspace --exclude moonbridge-app

# 前端：安装 + 构建
pnpm --dir ui install
pnpm --dir ui build

# 前端：纯浏览器开发（无需 Tauri 壳，默认连本地真实服务端）
pnpm --dir ui dev
# 浏览器访问 http://localhost:5173，vite 把 /api 代理到 127.0.0.1:38440，
# 页面先弹登录卡填 admin token。VITE_USE_MOCK=1 pnpm --dir ui dev 切内存
# mock（不连后端），header 出现琥珀色「MOCK 数据」徽标

# 无头服务端（仅本地开发示例；生产应注入两个不同的随机 token）
cargo run -p moonbridge-server -- --admin-token dev-admin-token --gateway-token dev-gateway-token

# 桌面应用（开发热重载）：从项目根运行
cargo tauri dev

# 打包
cargo tauri build
```

几个说明：

- **前端运行时三态**：所有 command 调用收敛在 `ui/src/lib/api.ts` 的 `call()` 分发层——Tauri 壳内走真实 IPC；纯浏览器默认走 `ui/src/lib/web.ts` 的 REST 客户端（连真实服务端，token 仅存页面内存，刷新需重登，401 自动弹登录卡，页面配置 CSP）；仅 `VITE_USE_MOCK=1` 时切到 `ui/src/lib/mock.ts` 的内存实现（数据刷新即重置，写操作在会话内生效）。
- **pnpm 构建脚本**：若 pnpm 因供应链策略忽略 `esbuild/vue-demi` 的构建脚本而阻断运行，`ui/pnpm-workspace.yaml` 已设 `strictDepBuilds:false` 与 `verifyDepsBeforeRun:false`。
- 网关默认监听 `127.0.0.1:38440`；健康检查 `GET /health`，模型列表 `GET /v1/models` 继续公开（既有行为），Web 模式 POST 入口必须使用独立网关 token。

## Docker 与服务器部署

根目录 `Dockerfile` 多阶段构建（node 构建 ui → rust 编译 server → debian-slim 运行时，uid 10001，`EXPOSE 38440`，`VOLUME /data`）。先在启动环境提供两个不同的非空随机 token：

```bash
docker build -t moonbridge-next:local .
docker run -d -p 38440:38440 \
  -e MOONBRIDGE_ADMIN_TOKEN -e MOONBRIDGE_GATEWAY_TOKEN \
  -e MOONBRIDGE_KEY_FILE=/data/moonbridge.key \
  -v "$PWD/data:/data" --restart unless-stopped moonbridge-next:local
```

- `./data` 挂进容器前必须 `chown -R 10001:10001`（容器以 uid 10001 运行，否则无法写库与密钥）。
- 管理 token 由 `MOONBRIDGE_ADMIN_TOKEN` / `--admin-token` 注入，仅用于 `/api/*`，不保存、不回显。网关 token 由 `MOONBRIDGE_GATEWAY_TOKEN` / `--gateway-token` 或既有配置提供，空值或与管理 token 相同均拒绝启动。
- 从共享 token 版本迁移：原值放入 `MOONBRIDGE_GATEWAY_TOKEN`，另生成不同的管理 token，模型客户端无需改 key。读取配置返回 `authToken: null`；保存 `null` / 空值保留既有网关 token，显式更新可落盘；保存无关配置不会持久化环境覆盖值。
- 「重启网关」在进程内优雅排空、执行生命周期钩子后重新监听，不依赖外部 supervisor；状态报告实际绑定地址。每代仅一个配额调度器，重启取消旧任务。容器 restart 策略仍可用于异常退出恢复。
- 默认文件数据库以 AES-GCM 统一加密 Provider 凭据与配额查询配置，V13 事务迁移数据和加密元数据；密钥默认 `<db>.key`，可由 `--key-file` / `MOONBRIDGE_KEY_FILE` 指定。Unix `0600`，Windows 当前用户 DPAPI 包装，恢复受账户绑定限制。已有加密库缺失/错误密钥时拒绝打开。
- 备份数据库必须同时备份密钥。升级前另留数据库备份；旧镜像不能直接回滚新加密库，须恢复升级前数据库及匹配凭据材料。密钥不应提交版本库；字段加密不等于整库、配置或 trace 加密。
- 配额查询 HTTP 仅允许同源/授权 origin，默认拒绝私网/元数据地址。私网例外在启动时通过 `MOONBRIDGE_QUOTA_PRIVATE_ORIGINS` 精确 origin JSON 数组设置，不由配额配置授权，元数据永禁。不使用系统代理或重定向，DNS 校验后钉住地址；解压后响应上限 1 MiB，HTTP 30 秒、单 Provider 45 秒。显式 `egressProxy` 使配额 HTTP 明确报不支持代理，不会静默绕过；推理代理仍支持。
- soul 部署样例（compose + .env.example + 1panel-network）见 `deploy/soul/`。

## 验证命令与范围

以下是应执行的验证命令，不是通过记录。不保留会随代码变化失效的历史测试总数；报告验证结果时应明确是否包含桌面端、目标平台、浏览器与实际部署。

```bash
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets
cargo test --workspace
# 无桌面系统依赖时可排除 app，但需单独补桌面端验证
cargo test --workspace --exclude moonbridge-app
pnpm --dir ui type-check
pnpm --dir ui build
```

交付前还需验证真实双 token 鉴权与旧客户端迁移、配置读取/保存不泄漏凭据、刷新重登、密钥备份恢复及错密钥拒绝、进程内重启与调度器取消，以及配额 HTTP 的 origin/私网/元数据/代理/大小/超时边界；mock 和编译通过不能代替这些端到端检查。

写插件相关改动时，注意 `plugins/moonbridge.lua`（LSP stub）要与 `crates/plugin/src/{host,runtime,convert,bridge}.rs` 保持同步；示例插件 `plugins/examples/` 有加载回归测试，改钩子语义时顺手跑一下。

## 插件调试

- 插件日志走 tracing（target=`plugin`），带插件名，调日志级别用 `RUST_LOG` 即可。
- 开 trace（`GatewayConfig.trace_dir`）后，每次请求各阶段的原始报文都会落盘到 `<trace_dir>/<session>/<model>/`，是排查插件改写是否符合预期的最快手段。
