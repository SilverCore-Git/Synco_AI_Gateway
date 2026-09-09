# syntax=docker/dockerfile:1
#
# Image "tout-en-un" : Ollama + Synco AI Gateway dans le même conteneur, pour une installation en
# une seule commande (`docker run`). Pour un déploiement où Ollama et la passerelle doivent rester
# indépendants (mise à jour séparée, Ollama partagé par plusieurs services...), voir
# Dockerfile.gateway-only + docker-compose.example.yml.

# --- Build de la passerelle -------------------------------------------------
FROM rust:1-slim-bookworm AS builder

# cmake + gcc : requis pour compiler aws-lc-sys (backend crypto de rustls).
RUN apt-get update && apt-get install -y --no-install-recommends \
    cmake \
    gcc \
    pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src

RUN cargo build --release

# --- Image finale : celle d'Ollama, avec la passerelle ajoutée -------------
FROM ollama/ollama:latest

COPY --from=builder /app/target/release/synco_ai_gateway /usr/local/bin/synco_ai_gateway
COPY docker/entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh /usr/local/bin/synco_ai_gateway

ENV PORT=8787
ENV ALLOWED_ORIGIN=*
ENV RUST_LOG=info
# Modèle pré-téléchargé au démarrage (idempotent) — doit correspondre au champ "Modèle" saisi
# dans les paramètres IA de Synco (provider "gateway").
ENV OLLAMA_MODEL=llama3.2

EXPOSE 8787 11434

HEALTHCHECK --interval=30s --timeout=3s --start-period=5m --retries=3 \
    CMD bash -c 'echo > /dev/tcp/127.0.0.1/8787' || exit 1

ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
