# 需 Rust >= 1.85（部分依賴使用 edition2024）
FROM rust:1.88-bookworm AS builder
WORKDIR /app
COPY Cargo.toml ./
COPY src ./src
COPY static ./static
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /app/target/release/vps-monitor-server /app/server
COPY static /app/static
RUN mkdir -p /app/data
ENV DATABASE_URL=sqlite:/app/data/monitor.db
EXPOSE 8080
CMD ["/app/server"]
