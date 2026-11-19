# syntax=docker/dockerfile:1
#
# Image "tout-en-un" : Ollama + Synco AI Gateway dans le même conteneur, pour une installation en
# une seule commande (`docker run`). Pour un déploiement où Ollama et la passerelle doivent rester
# indépendants (mise à jour séparée, Ollama partagé par plusieurs services...), voir
# Dockerfile.gateway-only + docker-compose.example.yml.

# --- Build de la passerelle -------------------------------------------------
# Versions épinglées (pas de `latest`) : pour monter de version, changer le tag ici (et dans
# rust-toolchain.toml pour Rust), reconstruire et relancer `cargo test`. Tags vérifiés sur Docker Hub.
FROM rust:1.98.1-slim-bookworm AS builder

# cmake + gcc : requis pour compiler aws-lc-sys (backend crypto de rustls).
RUN apt-get update && apt-get install -y --no-install-recommends \
    cmake \
    gcc \
    pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src

RUN cargo build --release --locked

# --- Image finale : celle d'Ollama, avec la passerelle ajoutée -------------
# Ollama : version épinglée (tag publié le 2026-11-09). Montée de version : choisir un tag stable
# sur https://hub.docker.com/r/ollama/ollama/tags, reconstruire, vérifier `ollama pull`.
FROM docker.io/ollama/ollama:0.40.16

COPY --from=builder /app/target/release/synco_ai_gateway /usr/local/bin/synco_ai_gateway
COPY docker/entrypoint.sh /usr/local/bin/entrypoint.sh
# La passerelle tourne sans privilèges (l'entrypoint la lance via setpriv) ; Ollama garde root, car
# son volume de modèles existant est dans /root/.ollama.
RUN useradd --system --no-create-home --shell /usr/sbin/nologin gateway \
    && chmod +x /usr/local/bin/entrypoint.sh /usr/local/bin/synco_ai_gateway

ENV PORT=8787
# L'image d'Ollama fixe OLLAMA_HOST=0.0.0.0 : sans ceci Ollama serait joignable SANS authentification
# par tout conteneur du réseau, contournant la passerelle (/api/pull, /api/delete...). Il ne doit
# écouter que sur la boucle locale ; seule la passerelle (8787) est exposée.
ENV OLLAMA_HOST=127.0.0.1:11434
ENV RUST_LOG=info
# Modèle pré-téléchargé au démarrage (idempotent) ; c'est aussi, par défaut, le seul modèle que la
# passerelle accepte de relayer (SYNCO_ALLOWED_MODELS pour en autoriser d'autres).
# À fournir au lancement (la passerelle refuse de démarrer sans eux) : SYNCO_API_URL,
# SYNCO_GATEWAY_URL, SYNCO_ALLOWED_ORGS, et ALLOWED_ORIGIN pour l'accès depuis un navigateur.
ENV OLLAMA_MODEL=llama3.2

EXPOSE 8787

HEALTHCHECK --interval=30s --timeout=3s --start-period=5m --retries=3 \
    CMD bash -c 'echo > /dev/tcp/127.0.0.1/8787' || exit 1

ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
