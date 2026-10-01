# syntax=docker/dockerfile:1.7
# 需 Rust >= 1.85（部分依賴使用 edition2024）
FROM rust:1.88-bookworm AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY static ./static
# --mount=type=cache 把 cargo registry 和 target/ 目錄留在 BuildKit 的 cache
# 裡，跨次建置不會被清掉；CI 那邊再用 cache-to/cache-from: type=gha 把這個
# cache 存進 GitHub Actions，這樣沒改 Cargo.toml/Cargo.lock 時，axum/sqlx/
# reqwest/argon2 這些依賴就不用每次都重新編譯。注意：cache mount 裡的東西
# 不會進最終 image layer，所以編譯完一定要把 binary cp 到 cache 外面的路徑，
# 這裡選 /app/out/。
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/app/target \
    cargo build --release --locked && \
    mkdir -p /app/out && cp target/release/vps-monitor-server /app/out/

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates curl && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /app/out/vps-monitor-server /app/server
COPY static /app/static
RUN mkdir -p /app/data
ENV DATABASE_URL=sqlite:/app/data/monitor.db
# CI 建置時會用 --build-arg 帶進目前的 git sha（例如 sha-7bb1105），讓伺服器
# 自己知道現在跑的是哪個版本，/api/version 會原樣回傳這個值；admin.html
# 「新增機器」產生 compose 片段時，就是抓這個版本號去釘住 Agent 映像的 tag，
# 而不是永遠用會一直變的 :latest。本機用 `docker build` 沒帶這個參數時預設
# 是 "dev"，admin.html 看到 "dev" 會自動退回用 :latest。
ARG APP_VERSION=dev
ENV APP_VERSION=$APP_VERSION
EXPOSE 8080
# 內部固定聽 8080（對外映射到哪個 host port 由 docker-compose 決定，跟這裡
# 無關），健康檢查固定打容器內部的這個位址就好。
HEALTHCHECK --interval=30s --timeout=5s --start-period=15s --retries=3 \
  CMD curl -fsS http://localhost:8080/healthz || exit 1
CMD ["/app/server"]
