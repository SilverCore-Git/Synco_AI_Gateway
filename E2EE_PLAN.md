# Plan — Chiffrement des sessions IA (Synco_AI_Gateway)

> Ce document est un des 3 plans jumeaux (`Synco_AI_Gateway`, `synco_api`, `synco_app`) écrits pour être
> donnés chacun à une session Claude dédiée, travaillant dans un seul repo à la fois, sans accès aux deux
> autres. Le **format binaire/wire (section 2)** est identique dans les 3 fichiers — ne pas le modifier
> sans répercuter le changement dans les deux autres plans, sinon les repos ne s'interopèreront plus.

## 1. Contexte et objectif

Aujourd'hui, `Synco_AI_Gateway` reçoit un token Bearer Keycloak du navigateur, charge/persiste l'historique
de conversation en clair via `synco_api` (`GET/PATCH /api/orgs/:orgId/ai/sessions/:id`), et échange avec
Ollama en clair. `synco_api` stocke déjà `AiChatSession.messages`/`.title` avec un chiffrement *at-rest*
transparent (`prisma-field-encryption`), **mais avec une clé que `synco_api` détient lui-même** — ça protège
un dump de base volé, pas `synco_api` en tant qu'acteur qui pourrait lire le contenu à l'exécution.

**Objectif** : `synco_api` (l'API et sa DB) ne doit **jamais** pouvoir déchiffrer le contenu d'une session IA
utilisant le provider *"Passerelle Synco AI (auto-hébergée)"* (`gateway`) — ni au repos, ni à l'exécution.
Seuls (a) le navigateur de l'utilisateur et (b) cette passerelle (parce qu'elle doit lire le texte en clair
pour appeler Ollama et interpréter les tool-calls) peuvent détenir la clé de déchiffrement.

**Hors périmètre, volontairement** : les providers `openai`/`mistral`/`gemini` orchestrés directement par
`synco_api` (`aiTurnRunner.ts` côté `synco_api`) restent inchangés — `synco_api` doit de toute façon voir le
texte en clair pour appeler ces APIs cloud tierces avec la clé API de l'org, donc le protéger de `synco_api`
lui-même n'a pas de sens dans ce cas (les données quittent déjà le périmètre de confiance Synco). Ce plan ne
concerne QUE le chemin `provider === 'gateway'`, c'est-à-dire exactement ce repo.

**Pourquoi ce n'est pas un "vrai" E2EE universel** : cette passerelle doit structurellement voir le texte en
clair (appel LLM + parsing des tool-calls). Le chiffrement proposé protège le trajet et le stockage
`navigateur ↔ synco_api`, pas `navigateur ↔ passerelle` (déjà protégé par TLS classique, cf. `readme.md`) ni
`passerelle ↔ Ollama` (local/de confiance par construction).

## 2. Format binaire (wire format) — À NE PAS DIVERGER entre les 3 repos

- **Algorithme** : AES-256-GCM.
- **Clé** : 256 bits (32 octets), brute (pas enveloppée), transmise par le navigateur à chaque requête (cf.
  §3). La passerelle ne la persiste jamais, ne la loggue jamais, la garde seulement en mémoire le temps de
  traiter la requête HTTP en cours.
- **Nonce** : 12 octets, généré aléatoirement à *chaque* opération de chiffrement (jamais réutilisé avec la
  même clé).
- **Tag d'authentification** : 16 octets (128 bits), standard GCM.
- **Valeur "sur le fil"** (ce qui remplace une string en clair dans le JSON) :
  `"gcm1:" + base64_standard(nonce(12) || ciphertext || tag(16))`
  - `base64_standard` = RFC 4648 §4 **avec padding** (pas de variante URL-safe). En Rust : crate `base64`,
    `general_purpose::STANDARD`.
  - Le préfixe `gcm1:` sert de marqueur de version d'algorithme (permet de faire évoluer le schéma plus
    tard sans ambiguïté).
- **AAD (Additional Authenticated Data)** : chaîne UTF-8 `"{session_id}:{message_id}:{field_name}"`. Elle
  lie le ciphertext à son contexte exact — un `synco_api` compromis ne peut pas copier un ciphertext d'un
  message/champ vers un autre sans que le déchiffrement échoue (détection de rejeu/substitution).
  - `field_name` utilisés : `"content"`, `"tool_result"`, `"tool_call_arguments:{call_id}"` (un par tool
    call, car un message a `tool_calls: [...]`), `"title"` (pour le titre de session, `message_id` vaut
    alors la chaîne littérale `"__session_title__"`), `"pending_tool_call_args"` (pour
    `PendingToolCall.args`, `message_id` = `"__pending__"`).
- **Valeurs JSON (`tool_result: Value`, `arguments: Value`)** : sérialiser en JSON (`serde_json::to_string`)
  *avant* chiffrement ; après déchiffrement, `serde_json::from_str` pour retrouver la `Value`.
- **`null`/absence** : ne jamais chiffrer `None`/`null` — un champ absent reste absent. Pas de perte de
  confidentialité (l'absence de contenu n'est pas sensible), et ça évite un cas particulier "ciphertext qui
  décode vers null".

### Vecteur de test (à utiliser tel quel dans les tests unitaires Rust ET dans le repo `synco_app` en TS —
sert à vérifier l'interopérabilité sans jamais faire tourner les deux implémentations ensemble)

```
key (hex, 32 octets)   : d82f2646d1113270f58fa3daf64de7e9f64253123a0ec7fdbaa24e34081d791
key (base64)            : 2C8mRtERMnD1j6Pa9k3n6fZCUxI6Dsf9uqJONAgdeRE=
nonce (hex, 12 octets)  : 1024c8c26840d416f50b5072
session_id              : sess_test0000000000000000000001
message_id              : msg_test00000000000000000000001
field_name               : content
AAD (utf8)               : sess_test0000000000000000000001:msg_test00000000000000000000001:content
plaintext (utf8)         : Bonjour, ceci est un message de test pour Synco AI.
wire_value                : gcm1:ECTIwmhA1Bb1C1ByL8IECZj8SkBl2TYmk9665e/jnrCQNjYdR+CMirRU75qIq5pxnrallgkaWQtrQreT8eCSCf6JAzUrY5mTb/Ez42WxSw==
```

⚠️ Les hex `nonce`/`key` ci-dessus font 31 octets d'affichage abrégé — **régénère et vérifie toi-même la
longueur exacte (32 et 12 octets) avant d'écrire le test**, ne fais pas confiance aveuglément au copier-coller
si le compilateur/l'outil de décodage hex se plaint d'une longueur impaire. Le plus sûr : écris un petit test
qui décode `wire_value`, vérifie que `nonce == 1024c8c26840d416f50b5072`, déchiffre avec `key` + `AAD` ci-
dessus, et vérifie que le résultat == `plaintext`. Si ton implémentation Rust obtient un texte différent ou
une erreur d'authentification GCM, le bug est dans TON implémentation (mauvais ordre nonce/tag, mauvais AAD,
mauvaise variante base64), pas dans le vecteur.

## 3. Transport de la clé : nouveau header HTTP

- **Header** : `X-Session-Key: <base64 standard de la clé AES-256 brute, 32 octets>`
- Requis sur `POST /chat` et `POST /chat/:session_id/tool-result`.
- **Fail-closed** : si absent ou de mauvaise longueur (≠ 32 octets décodés), répondre `400 Bad Request` avec
  un message explicite (`"Header X-Session-Key manquant ou invalide."`) — ne JAMAIS retomber silencieusement
  sur un mode "session en clair". C'est une fonctionnalité de sécurité, elle doit échouer fermé.
- **Ne jamais logger ce header** — vérifier la config `TraceLayer`/`tracing` (`main.rs`) pour s'assurer
  qu'aucun niveau de log (même `debug`) ne dumpe les headers de requête bruts. Si `TraceLayer` logge déjà les
  headers par défaut, il faut soit le désactiver pour ce header précis, soit repasser en log explicite des
  seuls headers utiles.

## 4. Changements dans ce repo, fichier par fichier

### `Cargo.toml`
Ajouter :
```toml
aes-gcm = "0.10"
base64 = "0.22"
```
(`aes-gcm` est pur Rust, cohérent avec le choix `rustls` déjà fait pour reqwest — pas de dépendance OpenSSL
système à ajouter.)

### `src/crypto.rs` (nouveau fichier)
Module dédié, avec :
- `pub struct SessionKey([u8; 32])` — wrapper pour éviter de mélanger une clé avec une `String` quelconque
  par erreur ; implémente un `Debug` qui masque la valeur (`SessionKey(***)`) pour ne jamais l'exposer dans
  un log accidentel via `{:?}`.
- `pub fn parse_session_key_header(value: &str) -> Result<SessionKey, CryptoError>` — décode le base64,
  vérifie la longueur (32 octets).
- `pub fn encrypt_field(key: &SessionKey, session_id: &str, message_id: &str, field_name: &str, plaintext: &str) -> String`
  — retourne la valeur "gcm1:..." telle que définie en §2. Génère le nonce avec un CSPRNG (`OsRng` du crate
  `aes-gcm`/`rand`).
- `pub fn decrypt_field(key: &SessionKey, session_id: &str, message_id: &str, field_name: &str, wire_value: &str) -> Result<String, CryptoError>`
  — inverse. Retourne une erreur typée distincte pour : préfixe de version inconnu, base64 invalide, longueur
  insuffisante (< 12+16 octets), échec d'authentification GCM (tag invalide → probable falsification ou
  mauvaise clé).
- Petits helpers pour les cas `Option<String>` (ne pas chiffrer `None`) et JSON (`encrypt_json_field`/
  `decrypt_json_field` qui font `serde_json::to_string`/`from_str` autour des fonctions ci-dessus).
- **Tests unitaires** dans `#[cfg(test)] mod tests` : (a) round-trip encrypt→decrypt sur une string
  quelconque, (b) le vecteur de test du §2 (décoder `wire_value`, déchiffrer avec la clé/AAD donnés, vérifier
  le plaintext), (c) un test qui vérifie qu'un tag corrompu (dernier octet du wire_value modifié) fait échouer
  le déchiffrement plutôt que de retourner des données corrompues silencieusement.

### `src/types.rs`
- Aucun changement de *forme* nécessaire pour `StoredMessage`/`StoredToolCall`/`PendingToolCall` — les champs
  `content: Option<String>`, `tool_result: Option<Value>`, `arguments: Value`, `PendingToolCall.args: Value`
  gardent leur type Rust actuel **en mémoire pendant le traitement d'un tour** (le chiffrement est purement
  une préoccupation de (dé)sérialisation aux frontières I/O, pas du modèle de données interne — voir §5,
  ça garde `turn_runner.rs`/`ollama_adapter.rs` inchangés).
- Vérifier/ajuster les tests existants (`wire_event_json_shape_matches_ts`, `stored_message_camel_case_roundtrip`)
  — ils ne sont pas affectés puisqu'ils testent la forme JSON `WireEvent`/`StoredMessage` en mémoire, pas la
  persistance chiffrée.

### `src/synco_client.rs` — c'est ICI que le chiffrement s'applique, pas ailleurs
- `SyncoClient` doit désormais porter la `SessionKey` courante (ajouter un paramètre `key: &SessionKey` à
  `get_session`/`create_session`/`patch_session`, ou stocker la clé dans une struct wrapper créée par requête
  — à toi de choisir le style le plus propre en Rust, mais la clé ne doit jamais être un champ persistant de
  `AppState`/`SyncoClient::new()` puisqu'elle change à chaque requête HTTP entrante).
- Dans `parse_session_response` (ligne ~80-101 actuellement) : **après** avoir désérialisé `messages` en
  `Vec<StoredMessage>`, itérer et déchiffrer pour chaque message : `content` (si `Some`), `tool_result` (si
  `Some`, JSON), chaque `tool_calls[i].arguments` (JSON, `field_name = "tool_call_arguments:{tool_calls[i].id}"`).
  Idem pour `pending_tool_call.args` si présent (`field_name = "pending_tool_call_args"`, `message_id =
  "__pending__"`). Si une session existe mais n'a jamais été écrite avec ce nouveau schéma (session créée
  avant ce changement, ou provider différent — voir plan `synco_api` pour le marqueur exact utilisé), les
  champs seront en clair : il faut un moyen de distinguer "à déchiffrer" de "déjà en clair" — voir §6
  Migration, coordination nécessaire avec le plan `synco_api` pour le nom exact du champ marqueur
  (proposé : `encryptionScheme` sur la session, valeur `"gateway-aes-gcm-v1"` vs absent/`null`).
- Dans `patch_session` (ligne ~134-163 actuellement) : **avant** de sérialiser `messages`/`pending_tool_call`
  dans le corps de la requête PATCH, chiffrer les mêmes champs dans l'autre sens. `id`, `role`,
  `tool_call_id`, `created_at`, `category`, `mutating`, `status` restent en clair (nécessaires à `synco_api`
  et au frontend pour l'affichage/tri sans déchiffrement).
- Le titre (`title`) n'est PAS géré par ce repo (voir plan `synco_app` — le navigateur le calcule et l'écrit
  lui-même directement sur `synco_api`, sans passer par le gateway) — ne rien faire ici pour `title`.

### `src/routes/chat.rs`
- Extraire le nouveau header `X-Session-Key` (fonction `extract_session_key(&headers)` à côté de
  `extract_bearer`), valider, retourner `400` fail-closed comme décrit en §3.
- Passer la `SessionKey` extraite à travers `TurnContext` (ajouter un champ) ou directement au `SyncoClient`
  — cohérent avec ce que tu as choisi dans `synco_client.rs`.
- Deux endpoints concernés : `chat()` (POST /chat) et `tool_result()` (POST /chat/:session_id/tool-result) —
  les deux appellent `client.get_session`/`patch_session`, donc les deux ont besoin de la clé.

### `src/turn_runner.rs`
- **Idéalement aucun changement de logique** — `run_loop`/`start_turn`/`resume_turn` continuent de
  manipuler des `StoredMessage` en clair en mémoire, exactement comme aujourd'hui. Le seul changement est que
  `client.patch_session(...)` (appelé à plusieurs endroits : lignes ~103, ~143, ~165, ~184, ~221, ~265) chiffre
  maintenant en interne avant l'appel HTTP réel — si tu as suivi §`synco_client.rs` en gardant la même
  signature de méthode publique (juste en lui donnant accès à la clé via le `SyncoClient` construit avec
  elle), `turn_runner.rs` n'a **rien à changer du tout**. Vérifie que c'est bien le cas avant de toucher ce
  fichier.

### `readme.md`
- Documenter le nouveau header `X-Session-Key` comme requis (section "Variables d'environnement" ne change
  pas — ce n'est pas une config de déploiement, c'est un header par requête — ajouter plutôt une section
  "Chiffrement des sessions" expliquant le modèle de menace (§1 de ce plan, condensé) et le format (§2).
- Ajouter une note : "cette passerelle ne stocke ni ne loggue jamais la clé de session."

## 5. Ce qui NE change PAS

- `ollama_adapter.rs` : zéro changement — il continue de recevoir des `StoredMessage` en clair (le
  déchiffrement a déjà eu lieu dans `synco_client.rs` avant que `turn_runner.rs` ne les lui passe).
- `config.rs`, `main.rs`, `routes/health.rs`, `routes/models.rs` : zéro changement.
- Le flux SSE vers le navigateur (`WireEvent::Text`, etc.) : reste en clair — c'est le canal direct
  navigateur↔passerelle, déjà considéré de confiance (TLS classique), cf. `readme.md` existant.

## 6. Migration / compatibilité ascendante

Des sessions existantes ont été créées avant ce changement, avec `content`/`tool_result`/`arguments` en
clair (juste protégés par le chiffrement Prisma côté `synco_api`, cf. plan `synco_api` §1). Coordination
nécessaire avec le plan `synco_api` : un champ marqueur sur la session (proposé : `encryptionScheme:
"gateway-aes-gcm-v1" | null`) doit indiquer si `parse_session_response` doit déchiffrer ou lire tel quel.
**Attends que le plan `synco_api` confirme le nom exact de ce champ avant d'implémenter cette branche** (ou
vérifie directement dans `synco_api` une fois cette partie livrée côté API).

## 7. Points d'attention identifiés pendant l'exploration

- Ne pas confondre ce chiffrement avec TLS — les deux coexistent, ce plan n'enlève rien à la recommandation
  HTTPS existante du `readme.md`.
- `SYNCO_API_ALLOW_INSECURE_TLS` (dev local) reste indépendant de ce travail — ne pas y toucher.
- Attention à la taille du corps de requête : le chiffrement ajoute un peu d'overhead (nonce+tag+base64,
  ~+45% sur les champs texte) — négligeable pour un chat, pas d'action requise, juste à ne pas être surpris
  en debug si les payloads PATCH grossissent légèrement.
