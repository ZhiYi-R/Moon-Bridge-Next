# moonbridge-server 生产镜像（多阶段：前端 → Rust 服务端 → 精简运行时）
#
# 构建（仓库根目录）：
#   docker build -t moonbridge-next:local .
#   docker build -t moonbridge-next:local --build-arg NPM_REGISTRY=https://registry.npmjs.org .
#
# 运行必需 MOONBRIDGE_ADMIN_TOKEN（见 deploy/soul/.env.example）。
# 产物在同一端口同时提供：LLM 网关 + 管理 REST API（/api/*）+ 前端静态页。

# ---- 阶段 1：前端静态产物 ----
FROM node:20-bookworm-slim AS ui

# 依赖源可用 --build-arg 覆盖：若镜像源拉取 corepack/pnpm 失败，改用
# --build-arg NPM_REGISTRY=https://registry.npmjs.org。
ARG NPM_REGISTRY=https://registry.npmmirror.com
# node:20 自带的 pnpm 9 不认识 ui/pnpm-workspace.yaml 的 onlyBuiltDependencies；
# 固定到已在仓库验证过的 pnpm 10（lockfileVersion 9.0 兼容）。
ARG PNPM_VERSION=10.34.5

# corepack 下载 pnpm 走这个变量（npm config 只影响 npm 自身）。
ENV COREPACK_NPM_REGISTRY=${NPM_REGISTRY}

WORKDIR /ui

RUN corepack enable \
    && corepack prepare pnpm@${PNPM_VERSION} --activate \
    && npm config set registry ${NPM_REGISTRY}

# 依赖清单单独一层：清单不变时复用缓存，改业务代码不重装依赖。
COPY ui/package.json ui/pnpm-lock.yaml ui/pnpm-workspace.yaml ./
RUN pnpm install --frozen-lockfile

COPY ui/ ./
RUN pnpm build

# ---- 阶段 2：服务端二进制 ----
# 锁定依赖（icu_properties 2.3 等）要求 rustc ≥1.88；与 kubuntu 验证过的工具链 1.95 对齐。
FROM rust:1.95-bookworm AS rs

# rsproxy 镜像源（国内构建机加速）；直连 crates.io 时删掉本 RUN 即可。
RUN printf '%s\n' \
    '[source.crates-io]' \
    'replace-with = "rsproxy-sparse"' \
    '' \
    '[source.rsproxy-sparse]' \
    'registry = "sparse+https://rsproxy.cn/index/"' \
    '' \
    '[net]' \
    'git-fetch-with-cli = true' \
    > /usr/local/cargo/config.toml

WORKDIR /build

# 整个 workspace 一次拷入（.dockerignore 已排除 target/node_modules 等：
# crates/server 是 workspace 成员，缺失成员清单会让 cargo 直接报错）。
COPY . .

# 只构建纯服务端二进制：不依赖 tauri/WebKit，mlua(vendored)/rusqlite(bundled)
# 需要 C 编译器，本镜像自带。
RUN cargo build --release -p moonbridge-server

# ---- 阶段 3：运行时 ----
FROM debian:bookworm-slim

# 运行时依赖：出网访问 models.dev / 上游 API 用的 CA 证书。
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# 非 root 运行。uid 与 deploy/soul 的 /data 目录所有权保持一致。
RUN groupadd --gid 10001 moonbridge \
    && useradd --uid 10001 --gid 10001 --no-create-home --shell /usr/sbin/nologin moonbridge

WORKDIR /app
# 注意：COPY 到 /app 会顺带把 /app 的所有者留成 root；运行期只有 /data 需要写，
# 二进制与前端产物保持 root 只读即可。
COPY --from=rs /build/target/release/moonbridge-server /app/moonbridge-server
COPY --from=ui /ui/dist /app/ui/dist

# /data 承载持久化数据：moonbridge.db、config/config.toml、插件与 trace 等。
RUN mkdir -p /data/config \
    && chown -R 10001:10001 /data

USER moonbridge

EXPOSE 38440
VOLUME /data

# 前端静态产物目录（server 模式托管管理页面）。
ENV MOONBRIDGE_WEB_DIR=/app/ui/dist

ENTRYPOINT ["/app/moonbridge-server"]
CMD ["--addr", "0.0.0.0:38440", "--config-dir", "/data/config", "--data-dir", "/data"]
