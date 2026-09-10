# Synco AI Gateway

Passerelle auto-hébergée qui donne à [Ollama](https://ollama.com) le vrai tool-calling de Synco AI (créer une tâche, lister des espaces, etc.), sans jamais faire transiter votre inférence par les serveurs centraux de Synco.

## Comment ça marche

Le navigateur de l'utilisateur appelle **cette passerelle directement**, pas l'API Synco. La passerelle :

1. Reçoit le même token Keycloak que le navigateur utilise déjà pour parler à l'API Synco — aucune clé à distribuer.
2. Charge/persiste la conversation en appelant l'API Synco (`GET`/`POST`/`PATCH /api/orgs/:orgId/ai/sessions/...`), en relayant ce token tel quel — c'est l'API Synco qui authentifie réellement chaque appel, la passerelle ne fait aucune vérification JWT locale.
3. Appelle votre Ollama (`/v1/chat/completions`, compatible OpenAI) avec l'historique et les tools disponibles (`GET /api/orgs/:orgId/ai/tools`), et diffuse la réponse en SSE au navigateur.
4. Quand un tool doit s'exécuter côté serveur (créer une tâche, etc.), elle appelle `POST /api/orgs/:orgId/ai/tools/:name/execute` sur l'API Synco — **toute la logique métier et les permissions restent dans Synco**, la passerelle n'a et ne doit jamais avoir d'accès direct à une base de données.

La passerelle est **sans état et sans configuration par organisation** : le modèle à utiliser est envoyé à chaque requête par le frontend (configuré dans les paramètres IA de l'organisation, provider *"Passerelle Synco AI (auto-hébergée)"*). Un seul déploiement peut donc servir n'importe quelle organisation qui le pointe correctement.

L'URL d'Ollama et l'URL de l'API Synco, elles, **ne sont pas des champs des paramètres Synco** — ce sont des variables d'environnement de la passerelle (`OLLAMA_URL`/`SYNCO_API_URL`, cf. plus bas), parce que ce sont des décisions de déploiement (où tourne Ollama, où joindre Synco **depuis la passerelle**) et non des décisions par organisation. C'est important : "localhost" ne désigne pas la même machine selon qu'on est dans le navigateur ou dans le conteneur de la passerelle — configurer ces deux variables au bon endroit évite exactement ce piège (voir `SYNCO_API_URL` plus bas si votre API Synco tourne sur la machine hôte pendant que la passerelle tourne dans Docker).

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
  -e SYNCO_API_URL="https://api.votre-synco.example" \
  synco-ai-gateway
```

- `-v synco_ollama_data:/root/.ollama` : indispensable — sans ce volume, le modèle (plusieurs Go) est retéléchargé à chaque recréation du conteneur.
- `-e OLLAMA_MODEL` : le modèle téléchargé automatiquement au démarrage (idempotent — ne re-télécharge rien s'il est déjà présent). Doit correspondre exactement au champ **Modèle** saisi dans les paramètres IA de Synco.
- `-e SYNCO_API_URL` : **important**, voir "Piège fréquent" ci-dessous.
- GPU Nvidia : ajoutez `--gpus all` (nécessite le [NVIDIA Container Toolkit](https://docs.nvidia.com/datacenter/cloud-native/container-toolkit/latest/install-guide.html) sur l'hôte).
- `docker logs -f synco-ai-gateway` pendant le premier démarrage : le téléchargement du modèle peut prendre plusieurs minutes selon sa taille et votre bande passante — c'est normal, le `HEALTHCHECK` laisse 5 minutes avant de s'inquiéter.

Une fois `docker logs` calme (Ollama et la passerelle prêts), configurez Synco avec l'URL publique de ce conteneur pour la passerelle — c'est le seul champ à renseigner, Ollama vit dans le même conteneur et la passerelle le contacte toute seule en local.

#### Piège fréquent : "localhost" ne veut pas dire la même chose partout

Si votre API Synco tourne sur la **même machine** que celle où vous lancez `docker run`, mais **pas dans un conteneur** (ex: `synco_api` en développement local), `http://localhost:9000` fonctionne dans votre navigateur mais **pas** depuis l'intérieur du conteneur de la passerelle — "localhost" y désigne le conteneur lui-même, pas votre machine hôte. Symptôme : `Connection refused` dans l'erreur renvoyée par la passerelle.

La solution : utiliser l'adresse spéciale `host.docker.internal`, qui désigne la machine hôte depuis n'importe quel conteneur.

```bash
docker run -d \
  --name synco-ai-gateway \
  --add-host=host.docker.internal:host-gateway \
  -p 8787:8787 \
  -v synco_ollama_data:/root/.ollama \
  -e SYNCO_API_URL="https://host.docker.internal:9000" \
  -e SYNCO_API_ALLOW_INSECURE_TLS=true \
  synco-ai-gateway
```

`--add-host=host.docker.internal:host-gateway` est nécessaire sur Linux (Docker Desktop sur Mac/Windows le fournit déjà automatiquement). `SYNCO_API_ALLOW_INSECURE_TLS=true` est probablement aussi nécessaire si votre `synco_api` local utilise son certificat auto-signé par défaut (voir plus bas). En production, où l'API Synco a une vraie adresse publique en HTTPS, aucun de ces deux réglages n'est nécessaire.

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
| `SYNCO_API_URL`  | *(aucune — retombe sur la valeur envoyée par le frontend)* | Où joindre l'API Synco **depuis la passerelle** — pas depuis le navigateur, voir "Piège fréquent" ci-dessus. Fortement recommandé de la définir explicitement dès que la passerelle tourne dans un conteneur. |
| `SYNCO_API_ALLOW_INSECURE_TLS` | `false` (désactivé) | **Développement local uniquement.** Si votre API Synco tourne en HTTPS avec un certificat auto-signé (le cas par défaut de `synco_api` en local), rustls le rejette toujours — même après avoir cliqué "continuer" dans un navigateur. Mettre à `true` désactive la vérification du certificat pour les appels vers l'API Synco. Ne jamais activer en production. |
| `RUST_LOG`       | `info`                     | Niveau de log (`tracing_subscriber::EnvFilter`, ex: `debug`, `synco_ai_gateway=debug`). |

## Chiffrement des sessions IA

Le contenu d'une session IA (messages, résultats d'outils, arguments d'appels d'outils) transite
en clair entre le navigateur et cette passerelle (protégé par TLS, cf. plus haut), mais est
**chiffré de bout en bout entre le navigateur et `synco_api`** : `synco_api` (l'API et sa base de
données) ne peut jamais déchiffrer ce contenu, ni au repos ni à l'exécution. Seuls le navigateur et
cette passerelle détiennent la clé — la passerelle doit voir le texte en clair pour appeler Ollama
et interpréter les tool-calls, mais **ne stocke ni ne loggue jamais la clé de session**.

- **Header requis** : `X-Session-Key: <base64 standard d'une clé AES-256 brute, 32 octets>`, sur
  `POST /chat` et `POST /chat/:session_id/tool-result`. Absent ou de mauvaise longueur → `400 Bad
  Request` (fail-closed, jamais de repli silencieux vers du clair).
- **Algorithme** : AES-256-GCM. Chaque valeur chiffrée a la forme
  `"gcm1:" + base64(nonce(12 octets) || ciphertext || tag(16 octets))`, avec un nonce aléatoire par
  opération et un AAD `"{sessionId}:{messageId}:{champ}"` qui lie chaque ciphertext à son contexte
  exact (un `synco_api` compromis ne peut pas rejouer un ciphertext d'un message/champ vers un
  autre).
- **Migration** : un champ non préfixé `"gcm1:"` est traité comme déjà en clair plutôt que comme
  une erreur — les sessions créées avant l'activation de ce chiffrement restent lisibles sans
  configuration supplémentaire.

Voir `E2EE_PLAN.md` pour le détail complet du format et du modèle de menace.

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
