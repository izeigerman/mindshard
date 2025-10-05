FROM rust:1.90 AS builder

WORKDIR /usr/src/app

COPY Cargo.toml Cargo.lock ./

COPY src ./src
COPY tests ./tests

RUN cargo build --release

RUN strip target/release/mindshard

FROM ubuntu:24.04

RUN apt-get update && apt-get install -y \
    --no-install-recommends \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy the binary from builder stage
COPY --from=builder /usr/src/app/target/release/mindshard /app/mindshard

ENV RUST_LOG=info
ENV MINDSHARD_PRIVATE_KEY_PATH=/app/certs/mindshard.key
ENV MINDSHARD_CA_CERT_PATH=/app/certs/mindshard.cer
ENV MINDSHARD_DB_PATH=/app/data/mindshard.db

EXPOSE 8080 3000

RUN mkdir -p /app/certs /app/data

CMD ["./mindshard"]