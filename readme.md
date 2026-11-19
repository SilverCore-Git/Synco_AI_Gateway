# Synco AI Gateway

Passerelle auto-hébergée qui **protège votre [Ollama](https://ollama.com)** : elle authentifie chaque appel avec un ticket signé par l'API Synco, puis relaie la requête LLM en flux. C'est tout : elle ne stocke rien, ne chiffre rien, ne connaît ni les conversations ni les outils de Synco AI.

## Rôle

```
navigateur (app Synco)  ──┐                                   ┌──▶ Ollama
                          ├─ Authorization: Bearer sgt1… ───▶ passerelle
runtime Synco AI        ──┘                                   └──  (jamais appelé sans ticket valide)
```

Le navigateur (boucle locale de l'app) et le **runtime Synco AI** appellent la passerelle de la même façon, toujours avec un **ticket `sgt1`** (jamais une clé API, jamais le jeton de connexion de l'utilisateur). La conversation, les outils, la persistance et le chiffrement vivent côté app, API et runtime.

Routes exposées (toute autre route répond `404` et n'atteint **jamais** Ollama : `/api/pull`, `/api/delete`, `/api/tags`, `/v1/embeddings`… sont inaccessibles) :

| Route | Ticket | Rôle |
|---|---|---|
| `GET /health` | non | Sonde de vie (`ok`). N'appelle pas Ollama. |
| `GET /models` | oui | `{ "models": [...] }` : modèles d'Ollama, restreints à `SYNCO_ALLOWED_MODELS` s'il est défini. |
| `POST /v1/chat/completions` | oui | Relais en flux vers Ollama (format OpenAI), corps plafonné et réécrit. |

### Corps relayé

Le corps (≤ `SYNCO_MAX_BODY_BYTES`, **1 Mio par défaut**, sinon `413`) est contrôlé puis réécrit avant d'atteindre Ollama. Ce plafond est configurable ; l'app Synco (boucle locale) doit s'y aligner : un historique avec des **images en base64** le dépasse vite, il faut alors soit relever `SYNCO_MAX_BODY_BYTES`, soit limiter la taille côté app. Le corps doit arriver en moins de 30 s (`408 body_timeout`).

- `model` doit être un modèle autorisé (`403 model_not_allowed` sinon) ;
- `max_tokens` (et `max_completion_tokens` s'il est fourni) est plafonné à `SYNCO_MAX_TOKENS` (4096 par défaut), `n` est forcé à 1 ;
- seuls les champs OpenAI usuels sont relayés (`messages`, `tools`, `tool_choice`, `stream`, `stream_options`, `temperature`, `top_p`, `stop`, `seed`, pénalités, `response_format`, `reasoning_effort`, `reasoning`) ; `options`, `keep_alive` et tout le reste sont supprimés ;
- seul `Content-Type` est transmis à Ollama : ni le ticket, ni aucun autre en-tête du client ;
- une réponse d'erreur d'Ollama n'est pas relayée : le client reçoit `502 {"error":"upstream_error"}` (ou `ollama_unavailable` si Ollama est injoignable), jamais le texte d'Ollama.

### Concurrence

Pour qu'un utilisateur (ou un runtime) ne puisse pas saturer Ollama, le relais limite les requêtes simultanées : `SYNCO_MAX_CONCURRENT_REQUESTS` au total (16 par défaut) et `SYNCO_MAX_CONCURRENT_PER_USER` par `sub` du ticket (4 par défaut). Au-delà : `429` avec `Retry-After: 1` et un code `too_many_requests_user` ou `too_many_requests` ; Ollama n'est pas appelé. La place est tenue pendant tout le flux et libérée à sa fin ou à la déconnexion du client (la requête vers Ollama est alors abandonnée).

### Authentification (ticket `sgt1`)

Format et règles : spécification du runtime (`synco_ai_runtime/docs/plan/02-tickets.md`). La passerelle vérifie **localement**, sans appel réseau par requête, dans cet ordre : longueur et forme, `kid` connu, signature Ed25519 (domaine `synco-ai-gateway-ticket\0`) avant tout décodage, JSON strict, fenêtre de validité (±30 s, durée ≤ 900 s), opération `llm`, `gw` = l'URL de **cette** passerelle (`SYNCO_GATEWAY_URL`), `org` ∈ `SYNCO_ALLOWED_ORGS`. Un ticket CDN (`sct1`) ou runtime (`srt1`) est refusé.

Chaque échec répond `401 {"error":"<code>"}` avec un code parmi `ticket_missing`, `ticket_invalid`, `ticket_unknown_key`, `ticket_expired`, `ticket_wrong_operation`, `ticket_wrong_audience`, `ticket_wrong_org`. Le client redemande un ticket une seule fois sur `ticket_expired`.

Clés publiques : lues sur `GET {SYNCO_API_URL}/api/ai/ticket-keys` au démarrage, toutes les 10 minutes, et sur un `kid` inconnu (au plus une fois par minute) : la rotation se fait donc sans redémarrage. **Échec fermé** : une réponse invalide n'écrase jamais le cache, et sans relecture réussie pendant 1 heure les clés ne sont plus utilisées (tout ticket est refusé). Si l'API est injoignable au démarrage, la passerelle démarre mais refuse tout ticket jusqu'à la première lecture réussie. La connexion à l'API est en HTTPS vérifié (HTTP accepté uniquement vers `localhost`), sans redirection.

### Journaux

Au niveau `info` (défaut), la passerelle n'écrit rien par requête : seulement le démarrage et les avertissements (clés illisibles, configuration). La trace HTTP (`TraceLayer`, niveau `DEBUG`) ne contient que la méthode et le **chemin** (jamais la requête de l'URL, donc jamais un ticket passé en `?ticket=`), les statuts et les durées. Aucun prompt, aucune réponse, aucun ticket (un test le vérifie avec un abonné `TRACE`).

## Variables d'environnement

| Variable | Défaut | Description |
|---|---|---|
| `SYNCO_API_URL` | **requise** | API Synco, jointe depuis la passerelle ; sert uniquement à lire les clés publiques de tickets. HTTPS (ou `http://localhost…` en développement). |
| `SYNCO_GATEWAY_URL` | **requise** | URL publique de cette passerelle, **exactement** celle saisie dans les paramètres IA de l'organisation (forme canonique : HTTPS, sans identifiants ni requête, sans `/` final). Doit égaler la revendication `gw` des tickets. |
| `SYNCO_ALLOWED_ORGS` | **requise, non vide** | Identifiants d'organisations Synco admises, séparés par des virgules (02 §5 étape 8). Sans cette liste, n'importe quelle organisation qui saisirait l'URL de votre passerelle pourrait obtenir des tickets pour elle ; il n'existe aucune option « toutes les organisations ». |
| `ALLOWED_ORIGIN` | *(aucune)* | Origine CORS de l'app Synco : **une seule** origine `scheme://host[:port]` en minuscules, sans chemin ni `/` final ni virgule (ex. `https://app.votre-synco.example`), sinon la passerelle refuse de démarrer. Sans valeur : aucun en-tête CORS (seul le runtime pourra appeler). `*` est accepté mais déconseillé. |
| `SYNCO_ALLOWED_MODELS` | `OLLAMA_MODEL`, sinon tout | Modèles relayés, séparés par des virgules. |
| `SYNCO_MAX_TOKENS` | `4096` | Plafond de `max_tokens`. |
| `SYNCO_MAX_BODY_BYTES` | `1048576` | Taille maximale du corps d'une requête (voir ci-dessus pour les images). |
| `SYNCO_MAX_CONCURRENT_REQUESTS` | `16` | Requêtes de chat simultanées, tous utilisateurs confondus. |
| `SYNCO_MAX_CONCURRENT_PER_USER` | `4` | Requêtes de chat simultanées par utilisateur. |
| `PORT` | `8787` | Port d'écoute. |
| `OLLAMA_URL` | `http://127.0.0.1:11434` | Où joindre Ollama (inchangé avec l'image tout-en-un). |
| `OLLAMA_MODEL` | `llama3.2` (image tout-en-un) | Modèle pré-téléchargé au démarrage de l'image tout-en-un. |
| `RUST_LOG` | `info` | Niveau de log. |

La passerelle refuse de démarrer si une variable requise manque ou est invalide. `SYNCO_API_ALLOW_INSECURE_TLS` n'existe plus : le certificat de l'API est toujours vérifié.

## Démarrage rapide (image tout-en-un)

`Dockerfile` : Ollama et la passerelle dans le même conteneur, un modèle pré-téléchargé au premier démarrage.

```bash
docker build -t synco-ai-gateway .

docker run -d \
  --name synco-ai-gateway \
  -p 8787:8787 \
  -v synco_ollama_data:/root/.ollama \
  -e OLLAMA_MODEL="llama3.2" \
  -e SYNCO_API_URL="https://api.votre-synco.example" \
  -e SYNCO_GATEWAY_URL="https://gateway.votre-synco.example" \
  -e SYNCO_ALLOWED_ORGS="identifiant-de-votre-organisation" \
  -e ALLOWED_ORIGIN="https://app.votre-synco.example" \
  synco-ai-gateway
```

- `-v synco_ollama_data:/root/.ollama` : indispensable, sinon le modèle (plusieurs Go) est retéléchargé à chaque recréation.
- `OLLAMA_MODEL` : modèle téléchargé au démarrage ; c'est aussi le seul modèle relayé par défaut.
- Sécurité : l'image force `OLLAMA_HOST=127.0.0.1:11434` (l'image d'Ollama écoute sinon sur toutes les interfaces) et n'expose que le port 8787 ; Ollama n'est donc joignable que par la passerelle. La passerelle tourne sous un utilisateur sans privilèges ; Ollama garde root à cause de son volume `/root/.ollama`.
- Versions épinglées (Ollama `0.40.16`, Rust `1.98.1`, Debian `bookworm-20261027`) : voir les commentaires des Dockerfiles pour monter de version.
- GPU Nvidia : `--gpus all` ([NVIDIA Container Toolkit](https://docs.nvidia.com/datacenter/cloud-native/container-toolkit/latest/install-guide.html)).
- Le `HEALTHCHECK` laisse 5 minutes au premier démarrage (téléchargement du modèle).
- Exposez la passerelle en **HTTPS** (reverse-proxy) : l'app Synco est en HTTPS et le navigateur bloque l'appel vers un `http://` (sauf `localhost`). Si votre API Synco tourne sur la machine hôte (développement), joignez-la depuis le conteneur via `host.docker.internal` (`--add-host=host.docker.internal:host-gateway` sous Linux) ; le certificat doit être valide.

### Ollama et passerelle séparés

`Dockerfile.gateway-only` (image passerelle seule, avec `HEALTHCHECK`) et `docker-compose.example.yml` :

```bash
cp docker-compose.example.yml docker-compose.yml
# éditez SYNCO_API_URL, SYNCO_GATEWAY_URL, SYNCO_ALLOWED_ORGS, ALLOWED_ORIGIN
docker compose up -d
docker compose exec ollama ollama pull llama3.1:8b
```

## Démarrage sans Docker

```bash
cargo build --release
SYNCO_API_URL=… SYNCO_GATEWAY_URL=… SYNCO_ALLOWED_ORGS=… ALLOWED_ORIGIN=… ./target/release/synco_ai_gateway
```

## Configuration côté Synco

Dans les paramètres IA de l'organisation, fournisseur **Passerelle Synco AI (auto-hébergée)** : renseignez l'URL de cette passerelle (identique à `SYNCO_GATEWAY_URL`) et le modèle. Aucune clé à saisir : l'API Synco émet pour chaque appel un ticket court (5 minutes) lié à cette URL, à l'utilisateur et à l'organisation.

## Migration d'un déploiement existant

Cette version **change l'API** de la passerelle :

1. **Routes retirées** : `POST /chat` et `POST /chat/{id}/tool-result` n'existent plus (404). La boucle d'agent (outils, persistance de la conversation) est désormais exécutée par l'app Synco (boucle locale) ou par le runtime Synco AI, qui appellent `POST /v1/chat/completions`. L'app Synco doit être à jour (lot L11 de la refonte) ; une app ancienne ne pourra plus utiliser ce fournisseur.
2. **Jeton Keycloak et `X-Session-Key`** : la passerelle ne reçoit plus ni l'un ni l'autre et ne parle plus à l'API Synco que pour lire les clés publiques.
3. **Variables à ajouter** avant la mise à jour (sinon la passerelle refuse de démarrer) : `SYNCO_GATEWAY_URL`, `SYNCO_ALLOWED_ORGS` (non vide). `SYNCO_API_URL` reste utilisé mais devient **requis** et ne sert plus qu'aux clés. `ALLOWED_ORIGIN` n'est plus `*` par défaut : sans lui, plus d'accès navigateur. `SYNCO_API_ALLOW_INSECURE_TLS` est supprimée.
4. **Conversations existantes** : les conversations créées avec la passerelle (chiffrement `gcm1:`, schéma `gateway-aes-gcm-v1`) restent stockées chiffrées par l'API et **restent lisibles dans l'app** : le déchiffrement est fait côté navigateur (format repris par la crate `synco-e2ee` et par l'app, lot L11). La passerelle n'y intervient plus ; son ancien module `crypto.rs` est supprimé.
5. Mettez à jour dans Synco l'URL de la passerelle si elle ne correspond pas exactement à `SYNCO_GATEWAY_URL` (forme canonique) : sinon tous les tickets seront refusés (`ticket_wrong_audience`).

## Développement

```bash
cargo test                                   # unitaires + intégration (faux Ollama, fausse API) + vecteurs
cargo clippy --all-targets -- -D warnings
cargo fmt --check                            # toolchain épinglée par rust-toolchain.toml (1.98.1)
```

`tests/vectors/tickets-gateway.json` est généré par le code réel de l'API (`synco_api/tests/vectors/ai-runtime/`, ne pas modifier à la main) ; `tests/vectors.rs` rejoue chaque cas et échoue au moindre écart avec le vérificateur de référence TypeScript.

## Hook git (secrets)

Le dépôt est public : `git config core.hooksPath .githooks` active le hook `pre-commit` qui exécute `scripts/check-secrets.mjs` (Node ou Bun requis) sur chaque commit. `node scripts/check-secrets.mjs --all` contrôle tout le dépôt.
