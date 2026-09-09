# syntax=docker/dockerfile:1

# --- Build stage -----------------------------------------------------------
FROM rust:1-slim-bookworm AS builder

# cmake + gcc: requis pour compiler aws-lc-sys (backend crypto de rustls).
RUN apt-get update && apt-get install -y --no-install-recommends \
    cmake \
    gcc \
    pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src

RUN cargo build --release

# --- Runtime stage -----------------------------------------------------------
FROM debian:bookworm-slim

# ca-certificates : rustls s'appuie sur le magasin de certificats système pour vérifier les
# endpoints HTTPS (synco_api en prod). Pas besoin d'OpenSSL — rustls est en pur Rust.
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

RUN useradd --system --no-create-home --shell /usr/sbin/nologin gateway

COPY --from=builder /app/target/release/synco_ai_gateway /usr/local/bin/synco_ai_gateway

USER gateway
EXPOSE 8787

ENV PORT=8787
ENV ALLOWED_ORIGIN=*
ENV RUST_LOG=info

ENTRYPOINT ["/usr/local/bin/synco_ai_gateway"]
