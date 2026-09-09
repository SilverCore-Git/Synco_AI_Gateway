# Synco AI Gateway

Passerelle auto-hébergée qui donne à [Ollama](https://ollama.com) le vrai tool-calling de Synco AI (créer une tâche, lister des espaces, etc.), sans jamais faire transiter votre inférence par les serveurs centraux de Synco.

## Comment ça marche

Le navigateur de l'utilisateur appelle **cette passerelle directement**, pas l'API Synco. La passerelle :

1. Reçoit le même token Keycloak que le navigateur utilise déjà pour parler à l'API Synco — aucune clé à distribuer.
2. Charge/persiste la conversation en appelant l'API Synco (`GET`/`POST`/`PATCH /api/orgs/:orgId/ai/sessions/...`), en relayant ce token tel quel — c'est l'API Synco qui authentifie réellement chaque appel, la passerelle ne fait aucune vérification JWT locale.
3. Appelle votre Ollama (`/v1/chat/completions`, compatible OpenAI) avec l'historique et les tools disponibles (`GET /api/orgs/:orgId/ai/tools`), et diffuse la réponse en SSE au navigateur.
4. Quand un tool doit s'exécuter côté serveur (créer une tâche, etc.), elle appelle `POST /api/orgs/:orgId/ai/tools/:name/execute` sur l'API Synco — **toute la logique métier et les permissions restent dans Synco**, la passerelle n'a et ne doit jamais avoir d'accès direct à une base de données.

La passerelle est **sans état et sans configuration par organisation** : l'URL de votre API Synco et le modèle à utiliser sont envoyés à chaque requête par le frontend (configurés dans les paramètres IA de l'organisation, provider *"Passerelle Synco AI (auto-hébergée)"*). Un seul déploiement peut donc servir n'importe quelle organisation qui le pointe correctement.

L'URL d'Ollama, elle, **n'est pas un champ des paramètres Synco** — c'est une variable d'environnement de la passerelle (`OLLAMA_URL`, cf. plus bas), parce que c'est une décision de déploiement (où tourne Ollama par rapport à cette passerelle) et non une décision par organisation. Par défaut `http://127.0.0.1:11434`, ce qui est toujours correct avec l'image tout-en-un.

## Avant de déployer : CORS et contenu mixte

- **CORS** : Ollama restreint par défaut les origines autorisées à l'appeler. Si Ollama tourne sur une machine différente de celle qui exécute cette passerelle, configurez `OLLAMA_ORIGINS` côté Ollama pour autoriser l'origine de cette passerelle.
- **Contenu mixte (HTTPS → HTTP)** : si votre instance Synco est servie en HTTPS (le cas normal), un navigateur bloque par défaut un appel vers une URL `http://` non sécurisée — **sauf** si la cible est `localhost`/`127.0.0.1`. Concrètement : passerelle sur la même machine que celle qui ouvre le navigateur → ça fonctionne même en HTTP. Passerelle sur une autre machine du réseau → il faut l'exposer en HTTPS (reverse-proxy avec un certificat, ex. Caddy/Traefik/nginx).

## Démarrage rapide (image tout-en-un)

L'image par défaut (`Dockerfile`) embarque **Ollama et la passerelle dans le même conteneur** — une seule commande, un modèle pré-téléchargé automatiquement au premier démarrage :

```bash
docker build -t synco-ai-gateway .

docker run -d \
  --name synco-ai-gateway \
  -p 8787:8787 \
  -v synco_ollama_data:/root/.ollama \
  -e ALLOWED_ORIGIN="https://app.votre-synco.example" \
  -e OLLAMA_MODEL="llama3.2" \
  synco-ai-gateway
```

- `-v synco_ollama_data:/root/.ollama` : indispensable — sans ce volume, le modèle (plusieurs Go) est retéléchargé à chaque recréation du conteneur.
- `-e OLLAMA_MODEL` : le modèle téléchargé automatiquement au démarrage (idempotent — ne re-télécharge rien s'il est déjà présent). Doit correspondre exactement au champ **Modèle** saisi dans les paramètres IA de Synco.
- GPU Nvidia : ajoutez `--gpus all` (nécessite le [NVIDIA Container Toolkit](https://docs.nvidia.com/datacenter/cloud-native/container-toolkit/latest/install-guide.html) sur l'hôte).
- `docker logs -f synco-ai-gateway` pendant le premier démarrage : le téléchargement du modèle peut prendre plusieurs minutes selon sa taille et votre bande passante — c'est normal, le `HEALTHCHECK` laisse 5 minutes avant de s'inquiéter.

Une fois `docker logs` calme (Ollama et la passerelle prêts), configurez Synco avec l'URL publique de ce conteneur pour la passerelle — c'est le seul champ à renseigner, Ollama vit dans le même conteneur et la passerelle le contacte toute seule en local.

### Déploiement avancé : Ollama et la passerelle séparés

Si vous voulez qu'Ollama et la passerelle puissent être mis à jour indépendamment, ou qu'un Ollama existant serve plusieurs services, utilisez plutôt `Dockerfile.gateway-only` (image passerelle seule) avec `docker-compose.example.yml` (Ollama dans un conteneur séparé) :

```bash
cp docker-compose.example.yml docker-compose.yml
# éditez ALLOWED_ORIGIN pour pointer vers l'origine de votre instance Synco
docker compose up -d
docker compose exec ollama ollama pull llama3.1:8b
```

## Démarrage sans Docker

```bash
cargo build --release
PORT=8787 ALLOWED_ORIGIN="https://app.votre-synco.example" ./target/release/synco_ai_gateway
```

## Variables d'environnement

| Variable         | Défaut                     | Description                                                                 |
|------------------|----------------------------|-------------------------------------------------------------------------------|
| `PORT`           | `8787`                     | Port d'écoute HTTP.                                                          |
| `ALLOWED_ORIGIN` | `*`                        | Origine CORS autorisée à appeler la passerelle depuis un navigateur.        |
| `OLLAMA_URL`     | `http://127.0.0.1:11434`   | Où joindre Ollama. Ne change jamais avec l'image tout-en-un ; à surcharger uniquement pour le déploiement avancé (Ollama dans un autre conteneur/machine). |
| `OLLAMA_MODEL`   | `llama3.2`                 | (Image tout-en-un uniquement) modèle pré-téléchargé au démarrage.            |
| `RUST_LOG`       | `info`                     | Niveau de log (`tracing_subscriber::EnvFilter`, ex: `debug`, `synco_ai_gateway=debug`). |

## Configuration côté Synco

Dans les paramètres IA de l'organisation, choisissez le provider **"Passerelle Synco AI (auto-hébergée)"** puis renseignez :

- **URL de votre Synco AI Gateway** — l'URL publique de ce service. C'est le seul champ requis.
- **Modèle** — sélectionné dans la liste des modèles déjà présents sur Ollama (récupérée automatiquement dès que l'URL de la passerelle est renseignée), ou saisi manuellement.

Aucune clé API à saisir : l'authentification passe par le compte Synco de l'utilisateur. Aucune URL Ollama à saisir non plus : c'est la passerelle qui sait où le trouver (cf. `OLLAMA_URL` ci-dessus).

## Développement

```bash
cargo build      # compiler
cargo test       # tests unitaires (format JSON des événements SSE, désérialisation)
cargo run        # lancer en local
```
