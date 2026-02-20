FROM rust:1.84 as builder
WORKDIR /app
COPY Cargo.toml Cargo.lock* ./
COPY src ./src
COPY migrations ./migrations
RUN cargo build --release

FROM debian:bookworm-slim
RUN useradd -m appuser
WORKDIR /app
COPY --from=builder /app/target/release/webhook-relay /usr/local/bin/webhook-relay
USER appuser
CMD ["webhook-relay"]
