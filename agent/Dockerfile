# syntax=docker/dockerfile:1.7
FROM rust:1.88-bookworm AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
# 跟 server/Dockerfile 同一套 cache mount 手法，見那邊的註解。
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/app/target \
    cargo build --release --locked && \
    mkdir -p /app/out && cp target/release/vps-monitor-agent /app/out/

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /app/out/vps-monitor-agent /app/agent
# 只在啟動時印一行版本方便對照，不影響任何行為（agent 本身不需要跟 server
# 版本比對什麼，是 server 那邊在生成新機器的 compose 片段時會去對齊版本）。
ARG APP_VERSION=dev
ENV APP_VERSION=$APP_VERSION
# agent 本身不聽 port，沒有 HTTP 端點可打，改成檢查 /tmp/agent-healthy 這個
# 心跳檔案（main.rs 每次上報成功就會更新它的時間戳）夠不夠新鮮。閾值取
# INTERVAL_SECS 的 3 倍、至少 90 秒，避免上報間隔設比較長時被誤判成不健康。
# coreutils（date/stat）在 debian-slim 是預裝的 essential 套件，不用額外裝。
HEALTHCHECK --interval=30s --timeout=5s --start-period=30s --retries=3 \
  CMD f=/tmp/agent-healthy; [ -f "$f" ] || exit 1; age=$(( $(date +%s) - $(stat -c %Y "$f") )); thr=$(( ${INTERVAL_SECS:-20} * 3 )); [ "$thr" -lt 90 ] && thr=90; [ "$age" -lt "$thr" ]
CMD ["/app/agent"]
