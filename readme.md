# Synco AI Gateway

Passerelle auto-hébergée qui donne à [Ollama](https://ollama.com) le vrai tool-calling de Synco AI (créer une tâche, lister des espaces, etc.), sans jamais faire transiter votre inférence par les serveurs centraux de Synco.

## Comment ça marche

Le navigateur de l'utilisateur appelle **cette passerelle directement**, pas l'API Synco. La passerelle :

1. Reçoit le même token Keycloak que le navigateur utilise déjà pour parler à l'API Synco — aucune clé à distribuer.
2. Charge/persiste la conversation en appelant l'API Synco (`GET`/`POST`/`PATCH /api/orgs/:orgId/ai/sessions/...`), en relayant ce token tel quel — c'est l'API Synco qui authentifie réellement chaque appel, la passerelle ne fait aucune vérification JWT locale.
3. Appelle votre Ollama (`/v1/chat/completions`, compatible OpenAI) avec l'historique et les tools disponibles (`GET /api/orgs/:orgId/ai/tools`), et diffuse la réponse en SSE au navigateur.
4. Quand un tool doit s'exécuter côté serveur (créer une tâche, etc.), elle appelle `POST /api/orgs/:orgId/ai/tools/:name/execute` sur l'API Synco — **toute la logique métier et les permissions restent dans Synco**, la passerelle n'a et ne doit jamais avoir d'accès direct à une base de données.

La passerelle est **sans état et sans configuration par organisation** : l'URL de votre API Synco, l'URL d'Ollama et le modèle à utiliser sont envoyés à chaque requête par le frontend (configurés dans les paramètres IA de l'organisation, provider *"Passerelle Synco AI (auto-hébergée)"*). Un seul déploiement peut donc servir n'importe quelle organisation qui le pointe correctement.

## Avant de déployer : CORS et contenu mixte

- **CORS** : Ollama restreint par défaut les origines autorisées à l'appeler. Si Ollama tourne sur une machine différente de celle qui exécute cette passerelle, configurez `OLLAMA_ORIGINS` côté Ollama pour autoriser l'origine de cette passerelle.
- **Contenu mixte (HTTPS → HTTP)** : si votre instance Synco est servie en HTTPS (le cas normal), un navigateur bloque par défaut un appel vers une URL `http://` non sécurisée — **sauf** si la cible est `localhost`/`127.0.0.1`. Concrètement : passerelle sur la même machine que celle qui ouvre le navigateur → ça fonctionne même en HTTP. Passerelle sur une autre machine du réseau → il faut l'exposer en HTTPS (reverse-proxy avec un certificat, ex. Caddy/Traefik/nginx).

## Démarrage rapide (Docker)

```bash
cp docker-compose.example.yml docker-compose.yml
# éditez ALLOWED_ORIGIN pour pointer vers l'origine de votre instance Synco
docker compose up -d
```

Le premier démarrage télécharge l'image Ollama ; pensez à `docker exec -it <container_ollama> ollama pull <modèle>` pour récupérer un modèle avant de configurer Synco.

## Démarrage sans Docker

```bash
cargo build --release
PORT=8787 ALLOWED_ORIGIN="https://app.votre-synco.example" ./target/release/synco_ai_gateway
```

## Variables d'environnement

| Variable         | Défaut  | Description                                                                 |
|------------------|---------|-------------------------------------------------------------------------------|
| `PORT`           | `8787`  | Port d'écoute HTTP.                                                          |
| `ALLOWED_ORIGIN` | `*`     | Origine CORS autorisée à appeler la passerelle depuis un navigateur.        |
| `RUST_LOG`       | `info`  | Niveau de log (`tracing_subscriber::EnvFilter`, ex: `debug`, `synco_ai_gateway=debug`). |

## Configuration côté Synco

Dans les paramètres IA de l'organisation, choisissez le provider **"Passerelle Synco AI (auto-hébergée)"** puis renseignez :

- **URL de votre Synco AI Gateway** — l'URL publique de ce service.
- **URL du serveur Ollama** — l'endpoint Ollama que la passerelle doit contacter.
- **Modèle** — l'identifiant du modèle Ollama à utiliser (ex. `llama3.1:8b`).

Aucune clé API à saisir : l'authentification passe par le compte Synco de l'utilisateur.

## Développement

```bash
cargo build      # compiler
cargo test       # tests unitaires (format JSON des événements SSE, désérialisation)
cargo run        # lancer en local
```
