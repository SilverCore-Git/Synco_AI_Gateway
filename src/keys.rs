//! Cache des clés publiques de l'API (02 §7) : lues au démarrage, toutes les 10 minutes et sur un
//! `kid` inconnu (au plus une fois par minute). Échec fermé : une réponse invalide n'écrase jamais
//! le cache, et un cache trop ancien est considéré comme vide.

use crate::ticket::{decode_b64u_exact, is_kid, KeyMap};
use std::sync::RwLock;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

const MAX_RESPONSE_BYTES: usize = 64 * 1024;
const MAX_KEYS: usize = 16;

#[derive(Clone, Debug)]
pub struct KeyStoreConfig {
    /// Rafraîchissement périodique (10 min).
    pub refresh_interval: Duration,
    /// Intervalle minimal entre deux relectures déclenchées par un `kid` inconnu (60 s).
    pub unknown_kid_min_interval: Duration,
    /// Au-delà, sans relecture réussie, les clés ne sont plus utilisées (échec fermé).
    pub max_stale: Duration,
}

impl Default for KeyStoreConfig {
    fn default() -> Self {
        Self {
            refresh_interval: Duration::from_secs(600),
            unknown_kid_min_interval: Duration::from_secs(60),
            max_stale: Duration::from_secs(3600),
        }
    }
}

struct Snapshot {
    keys: KeyMap,
    fetched_at: Instant,
}

pub struct KeyStore {
    http: reqwest::Client,
    url: String,
    config: KeyStoreConfig,
    snapshot: RwLock<Option<Snapshot>>,
    last_unknown_refresh: Mutex<Option<Instant>>,
}

/// Client HTTP dédié : pas de redirection, délais courts.
pub fn build_http_client() -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(10))
        .build()
}

/// Valide le corps de `GET /api/ai/ticket-keys` : `{ "v": 1, "keys": { kid: b64u(32) } }`.
pub fn parse_keys_document(body: &[u8]) -> Option<KeyMap> {
    let doc: serde_json::Value = serde_json::from_slice(body).ok()?;
    let obj = doc.as_object()?;
    if obj.len() != 2 || obj.get("v")?.as_u64() != Some(1) {
        return None;
    }
    let keys = obj.get("keys")?.as_object()?;
    if keys.is_empty() || keys.len() > MAX_KEYS {
        return None;
    }
    let mut out = KeyMap::new();
    for (kid, value) in keys {
        if !is_kid(kid) {
            return None;
        }
        let bytes = decode_b64u_exact(value.as_str()?, 32)?;
        out.insert(kid.clone(), bytes.try_into().ok()?);
    }
    Some(out)
}

impl KeyStore {
    pub fn new(http: reqwest::Client, api_url: &str, config: KeyStoreConfig) -> Self {
        Self {
            http,
            url: format!("{}/api/ai/ticket-keys", api_url.trim_end_matches('/')),
            config,
            snapshot: RwLock::new(None),
            last_unknown_refresh: Mutex::new(None),
        }
    }

    /// Clés utilisables maintenant (vide si le cache est absent ou périmé).
    pub fn current(&self) -> KeyMap {
        let Ok(guard) = self.snapshot.read() else {
            return KeyMap::new();
        };
        match guard.as_ref() {
            Some(s) if s.fetched_at.elapsed() <= self.config.max_stale => s.keys.clone(),
            _ => KeyMap::new(),
        }
    }

    /// Relit les clés ; en cas d'échec, le cache existant est conservé (jusqu'à `max_stale`).
    pub async fn refresh(&self) -> bool {
        match self.fetch().await {
            Some(keys) => {
                if let Ok(mut guard) = self.snapshot.write() {
                    *guard = Some(Snapshot {
                        keys,
                        fetched_at: Instant::now(),
                    });
                }
                true
            }
            None => {
                tracing::warn!("relecture des clés de tickets impossible ou invalide");
                false
            }
        }
    }

    async fn fetch(&self) -> Option<KeyMap> {
        let resp = self.http.get(&self.url).send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        if resp
            .content_length()
            .is_some_and(|n| n > MAX_RESPONSE_BYTES as u64)
        {
            return None;
        }
        let mut resp = resp;
        let mut body = Vec::new();
        while let Some(chunk) = resp.chunk().await.ok()? {
            body.extend_from_slice(&chunk);
            if body.len() > MAX_RESPONSE_BYTES {
                return None;
            }
        }
        parse_keys_document(&body)
    }

    /// Appelé sur un `kid` inconnu : relit les clés, au plus une fois par
    /// `unknown_kid_min_interval` (les tentatives échouées comptent). Les appels concurrents sont
    /// sérialisés : le second constate l'intervalle et ne relit pas.
    pub async fn refresh_for_unknown_kid(&self) {
        let mut last = self.last_unknown_refresh.lock().await;
        if last.is_some_and(|t| t.elapsed() < self.config.unknown_kid_min_interval) {
            return;
        }
        *last = Some(Instant::now());
        self.refresh().await;
    }

    /// Tâche de fond : relecture périodique.
    pub fn spawn_refresh_loop(self: &std::sync::Arc<Self>) {
        let store = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(store.config.refresh_interval).await;
                store.refresh().await;
            }
        });
    }
}
