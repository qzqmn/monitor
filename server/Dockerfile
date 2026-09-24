# 需 Rust >= 1.85（部分依賴使用 edition2024）
FROM rust:1.88-bookworm AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY static ./static
# --locked：嚴格按 Cargo.lock 解析依賴版本，CI 每次建置用的版本都一樣，
# 不會因為上游套件出新版就悄悄換掉依賴、甚至建置失敗。
RUN cargo build --release --locked

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /app/target/release/vps-monitor-server /app/server
COPY static /app/static
RUN mkdir -p /app/data
ENV DATABASE_URL=sqlite:/app/data/monitor.db
EXPOSE 8080
CMD ["/app/server"]
