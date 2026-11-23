# syntax=docker/dockerfile:1.7
#
# jumcp 生产镜像：Rust 后端（jumcp-server + jumcp-connector-clickhouse）+ Node 22 构建的管理台静态产物
# + 最小 Debian 运行时。仅支持 linux/amd64（发布流水线与 smoke 校验都按该平台固定）。
#
# 管理台是薄前端：镜像只把 admin-web/dist 以同源静态资源（JUMCP_MCP_WEB_DIR=/app/web）与 API 一起提供。
# 浏览器不发 Authorization，生产必须由网关/BFF 在同源路径注入 Bearer；镜像内不含任何 token/secret。

ARG RUST_IMAGE=rust:bookworm
ARG NODE_IMAGE=node:22-bookworm-slim
ARG RUNTIME_IMAGE=debian:bookworm-slim

# ---------------------------------------------------------------------------- 后端
FROM ${RUST_IMAGE} AS backend-builder
WORKDIR /build

# 先只拷清单再拷源码：源码变动时依赖产物仍命中 BuildKit 缓存挂载（target 目录跨构建复用）。
COPY backend/Cargo.toml backend/Cargo.lock ./
COPY backend/crates ./crates

RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,target=/build/target,sharing=locked \
    cargo build --release --locked -p jumcp-server -p jumcp-connector-clickhouse \
 && mkdir -p /out \
 && cp target/release/jumcp-server target/release/jumcp-connector-clickhouse /out/

# ---------------------------------------------------------------------------- 管理台
FROM ${NODE_IMAGE} AS web-builder
WORKDIR /build/admin-web

COPY admin-web/package.json admin-web/package-lock.json ./
RUN --mount=type=cache,target=/root/.npm,sharing=locked npm ci

COPY admin-web/ ./
RUN npm run build \
 && mkdir -p /out/web \
 && cp -R dist/. /out/web/

# ---------------------------------------------------------------------------- 运行时
FROM ${RUNTIME_IMAGE}

# 不带默认值重新声明：继承上面的全局默认或 --build-arg，供 base.name 标签写出真实基础镜像。
ARG RUNTIME_IMAGE
ARG VERSION=dev
ARG REVISION=unknown

LABEL org.opencontainers.image.title="jumcp" \
      org.opencontainers.image.description="jumcp MCP 聚合 router：Rust 后端 + 同源管理台静态资源" \
      org.opencontainers.image.source="https://github.com/Nnyjk/jumcp" \
      org.opencontainers.image.url="https://github.com/Nnyjk/jumcp" \
      org.opencontainers.image.documentation="https://github.com/Nnyjk/jumcp/blob/main/docs/ci-cd.md" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.revision="${REVISION}" \
      org.opencontainers.image.licenses="MIT" \
      org.opencontainers.image.base.name="${RUNTIME_IMAGE}"

# tini 负责 pid 1 的信号转发（STOPSIGNAL SIGTERM 才能真正优雅停机）；
# curl 供 HEALTHCHECK 使用；ca-certificates 供上游 MCP / ClickHouse 的 TLS 校验。
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl tini \
 && rm -rf /var/lib/apt/lists/* \
 && groupadd --gid 10001 jumcp \
 && useradd --uid 10001 --gid 10001 --home-dir /data --no-create-home --shell /usr/sbin/nologin jumcp \
 && install -d -o 10001 -g 10001 -m 0755 /app /app/config /app/web \
 && install -d -o 10001 -g 10001 -m 0750 /data

# 二进制放在同一目录：ConnectorExecutor 默认在可执行文件同目录查找 connector agent。
# 内置 MCP 包（clickhouse-basic）不再是独立二进制：它编译进 jumcp-server，经内嵌 HTTP 端点接入。
COPY --from=backend-builder /out/jumcp-server /out/jumcp-connector-clickhouse /usr/local/bin/
COPY --from=web-builder /out/web /app/web
COPY config/policy.toml /app/config/policy.toml

# 管理面账号（JUMCP_ADMIN_USERNAME / JUMCP_ADMIN_PASSWORD，Argon2id 落库）与两把密钥必须由运行时注入，
# 镜像里不设默认值：
# - JUMCP_SECRET_KEY（64 位 hex）：落库加密，内置包密码以 enc:v1 密文存进注册表；
# - JUMCP_TRANSPORT_KEY（64 位 hex）：请求体传输加密，管理台提交 secret 字段前用它加密。
# 缺任一密钥（或密钥非法）时 HTTP 模式拒绝启动，避免密码明文落盘或被明文提交。
# 空库里没有管理员时同样拒绝启动：首次部署必须显式给出初始账号与口令。
#
# 两个监听面（不能相同，相同即拒绝启动）：
# - JUMCP_MCP_PORT（MCP 面）：`/mcp`、`/internal/mcp/*`、`/health`，给 agent；
# - JUMCP_ADMIN_PORT（管理面）：`/admin/*`、`/health` 与管理台静态站（JUMCP_MCP_WEB_DIR），给浏览器。
# 管理台登录用 cookie 会话，因此还有三个可选旋钮：
# - JUMCP_ADMIN_SESSION_TTL_SECS（默认 43200 = 12 小时）；
# - JUMCP_ADMIN_COOKIE_SECURE=1 仅在 HTTPS（TLS 终止在网关时也算）下启用，否则浏览器不回传 cookie；
# - JUMCP_STATIC_AUDIT_INTERVAL_SECS（默认 300 秒，0 关闭）：后台预计算候选排名快照。
ENV JUMCP_MCP_WEB_DIR=/app/web \
    JUMCP_MCP_TRANSPORT=streamable-http \
    JUMCP_MCP_HOST=0.0.0.0 \
    JUMCP_MCP_PORT=8000 \
    JUMCP_ADMIN_PORT=8080 \
    JUMCP_MCP_CONFIG_FILE=/app/config/policy.toml \
    JUMCP_MCP_REGISTRY_PATH=/data/registry.sqlite3 \
    JUMCP_MCP_AUDIT_PATH=/data/audit.sqlite3 \
    RUST_LOG=info

WORKDIR /app
# 两个面都要发布：只开 8000 会让管理台登录页拿不到 cookie（另一个端口不可达）。
EXPOSE 8000 8080
VOLUME ["/data"]
USER 10001:10001
STOPSIGNAL SIGTERM

# 两个面各探一次：healthcheck 不该假设自己在哪个端口上（也顺手验证分面真的起了两个监听）。
HEALTHCHECK --interval=30s --timeout=5s --start-period=15s --retries=3 \
  CMD curl -fsS "http://127.0.0.1:${JUMCP_MCP_PORT:-8000}/health" -o /dev/null \
   && curl -fsS "http://127.0.0.1:${JUMCP_ADMIN_PORT:-8080}/health" -o /dev/null || exit 1

ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/jumcp-server"]
