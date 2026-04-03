FROM rust:1-slim AS builder

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src/ src/
COPY migrations/ migrations/

RUN cargo build --release

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/lean-ctx-cloud /usr/local/bin/lean-ctx-cloud

ENV PORT=3334
EXPOSE 3334

VOLUME ["/data"]
ENV LEAN_CTX_DB_PATH=/data/leanctx.db

CMD ["lean-ctx-cloud"]
