//! Banc d'essai : faux Ollama (qui compte ses appels), fausse API (clés publiques), passerelle
//! réelle sur un port local, signature de tickets de test.
#![allow(dead_code)]

use aws_lc_rs::signature::{Ed25519KeyPair, KeyPair};
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{any, get};
use axum::Router;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use synco_ai_gateway::app::{router, AppState};
use synco_ai_gateway::config::Config;
use synco_ai_gateway::keys::{build_http_client, KeyStore, KeyStoreConfig};
use synco_ai_gateway::ticket::Verifier;

pub const GATEWAY_URL: &str = "https://gateway.example.com";
pub const ORG: &str = "org_test_1";
pub const SUB: &str = "user_test_1";
pub const NOW_SKEW: i64 = 0;

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("horloge")
        .as_secs()
}

// ---------------------------------------------------------------- clés et tickets

pub struct TestKey {
    pub kid: String,
    pair: Ed25519KeyPair,
}

impl TestKey {
    pub fn new(kid: &str, seed_byte: u8) -> Self {
        Self {
            kid: kid.to_string(),
            pair: Ed25519KeyPair::from_seed_unchecked(&[seed_byte; 32]).expect("graine"),
        }
    }
    pub fn public_b64u(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.pair.public_key().as_ref())
    }
    /// Signature brute : préfixe et domaine au choix (cas négatifs de domaines croisés).
    pub fn sign_raw(&self, prefix: &str, domain: &[u8], payload_json: &str) -> String {
        let payload = URL_SAFE_NO_PAD.encode(payload_json.as_bytes());
        let signed = format!("{prefix}.{}.{payload}", self.kid);
        let mut msg = domain.to_vec();
        msg.extend_from_slice(signed.as_bytes());
        format!(
            "{signed}.{}",
            URL_SAFE_NO_PAD.encode(self.pair.sign(&msg).as_ref())
        )
    }
    pub fn gateway_ticket(&self, claims: &Claims) -> String {
        self.sign_raw("sgt1", b"synco-ai-gateway-ticket\0", &claims.json())
    }
}

pub struct Claims {
    pub op: &'static str,
    pub org: &'static str,
    pub gw: &'static str,
    pub iat: u64,
    pub exp: u64,
}

impl Claims {
    pub fn valid() -> Self {
        let n = now();
        Self {
            op: "llm",
            org: ORG,
            gw: GATEWAY_URL,
            iat: n,
            exp: n + 300,
        }
    }
    pub fn json(&self) -> String {
        format!(
            r#"{{"op":"{}","sub":"{SUB}","org":"{}","gw":"{}","iat":{},"exp":{}}}"#,
            self.op, self.org, self.gw, self.iat, self.exp
        )
    }
}

// ---------------------------------------------------------------- faux Ollama

#[derive(Default)]
pub struct OllamaSpy {
    pub calls: AtomicUsize,
    pub last_body: Mutex<Option<String>>,
    pub last_headers: Mutex<Option<HeaderMap>>,
    pub paths: Mutex<Vec<String>>,
    pub mode: Mutex<Mode>,
    /// Requêtes de chat en cours (futur du gestionnaire ou flux de réponse encore vivants).
    pub active: AtomicUsize,
}

#[derive(Clone, Copy, Default)]
pub enum Mode {
    #[default]
    Normal,
    /// Attend avant de répondre (en-têtes).
    SlowHeaders(u64),
    /// Répond tout de suite puis diffuse sans fin.
    Endless,
    /// Erreur d'Ollama avec un texte détaillé.
    ModelNotFound,
}

struct ActiveGuard(Arc<OllamaSpy>);
impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::SeqCst);
    }
}

pub struct FakeOllama {
    pub spy: Arc<OllamaSpy>,
    pub addr: std::net::SocketAddr,
}

impl FakeOllama {
    pub fn calls(&self) -> usize {
        self.spy.calls.load(Ordering::SeqCst)
    }
}

async fn ollama_handler(
    State(spy): State<Arc<OllamaSpy>>,
    uri: axum::http::Uri,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    spy.calls.fetch_add(1, Ordering::SeqCst);
    spy.paths
        .lock()
        .expect("verrou")
        .push(uri.path().to_string());
    *spy.last_body.lock().expect("verrou") = Some(String::from_utf8_lossy(&body).into_owned());
    *spy.last_headers.lock().expect("verrou") = Some(headers);
    if uri.path() == "/v1/chat/completions" {
        spy.active.fetch_add(1, Ordering::SeqCst);
        let guard = ActiveGuard(spy.clone());
        let mode = *spy.mode.lock().expect("verrou");
        match mode {
            Mode::SlowHeaders(ms) => tokio::time::sleep(Duration::from_millis(ms)).await,
            Mode::Endless => {
                let stream = futures_util::stream::unfold(guard, |g| async move {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    Some((
                        Ok::<_, std::convert::Infallible>(Bytes::from_static(
                            b"data: {\"x\":1}\n\n",
                        )),
                        g,
                    ))
                });
                return (
                    StatusCode::OK,
                    [("content-type", "text/event-stream")],
                    axum::body::Body::from_stream(stream),
                )
                    .into_response();
            }
            Mode::ModelNotFound => {
                return (
                    StatusCode::NOT_FOUND,
                    r#"{"error":"model 'secret-name' not found, try pulling it first"}"#,
                )
                    .into_response();
            }
            Mode::Normal => {}
        }
        drop(guard);
        return (
            StatusCode::OK,
            [("content-type", "text/event-stream")],
            "data: {\"choices\":[{\"delta\":{\"content\":\"salut\"}}]}\n\ndata: [DONE]\n\n"
                .to_string(),
        )
            .into_response();
    }
    match uri.path() {
        "/v1/models" => (
            StatusCode::OK,
            [("content-type", "application/json")],
            r#"{"object":"list","data":[{"id":"llama3.2"},{"id":"autre-modele"}]}"#.to_string(),
        )
            .into_response(),
        _ => (StatusCode::OK, "ollama").into_response(),
    }
}

pub async fn fake_ollama() -> FakeOllama {
    let spy = Arc::new(OllamaSpy::default());
    let app = Router::new()
        .fallback(any(ollama_handler))
        .with_state(spy.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move { axum::serve(listener, app).await.expect("serve") });
    FakeOllama { spy, addr }
}

// ---------------------------------------------------------------- fausse API

pub struct FakeApi {
    pub addr: std::net::SocketAddr,
    pub hits: Arc<AtomicUsize>,
    /// Corps renvoyé par `GET /api/ai/ticket-keys` et statut.
    pub response: Arc<Mutex<(u16, String)>>,
    /// Délai avant de répondre (la réponse est figée à l'arrivée de la requête).
    pub delay_ms: Arc<std::sync::atomic::AtomicU64>,
}

impl FakeApi {
    pub fn publish(&self, keys: &[&TestKey]) {
        let map: Vec<String> = keys
            .iter()
            .map(|k| format!(r#""{}":"{}""#, k.kid, k.public_b64u()))
            .collect();
        *self.response.lock().expect("verrou") =
            (200, format!(r#"{{"v":1,"keys":{{{}}}}}"#, map.join(",")));
    }
    pub fn set_raw(&self, status: u16, body: &str) {
        *self.response.lock().expect("verrou") = (status, body.to_string());
    }
    pub fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
}

pub async fn fake_api(keys: &[&TestKey]) -> FakeApi {
    let api = FakeApi {
        addr: ([127, 0, 0, 1], 0).into(),
        hits: Arc::new(AtomicUsize::new(0)),
        response: Arc::new(Mutex::new((200, String::new()))),
        delay_ms: Arc::new(std::sync::atomic::AtomicU64::new(0)),
    };
    api.publish(keys);
    let (hits, response, delay) = (api.hits.clone(), api.response.clone(), api.delay_ms.clone());
    let app = Router::new().route(
        "/api/ai/ticket-keys",
        get(move || {
            let (hits, response, delay) = (hits.clone(), response.clone(), delay.clone());
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                let (status, body) = response.lock().expect("verrou").clone();
                tokio::time::sleep(Duration::from_millis(delay.load(Ordering::SeqCst))).await;
                (
                    StatusCode::from_u16(status).expect("statut"),
                    [("content-type", "application/json")],
                    body,
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move { axum::serve(listener, app).await.expect("serve") });
    FakeApi { addr, ..api }
}

// ---------------------------------------------------------------- passerelle

pub struct Options {
    pub allowed_origin: Option<String>,
    pub allowed_models: Vec<String>,
    pub max_body_bytes: usize,
    pub keys: KeyStoreConfig,
    pub allowed_orgs: Vec<String>,
    pub max_concurrent: usize,
    pub max_per_user: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            allowed_origin: Some("https://app.example.com".into()),
            allowed_models: vec!["llama3.2".into()],
            max_body_bytes: 64 * 1024,
            keys: KeyStoreConfig::default(),
            allowed_orgs: vec![ORG.to_string()],
            max_concurrent: 16,
            max_per_user: 4,
        }
    }
}

pub struct Gateway {
    pub base: String,
    pub ollama: FakeOllama,
    pub api: FakeApi,
    pub keys: Arc<KeyStore>,
    pub http: reqwest::Client,
}

pub async fn start_gateway(key: &TestKey, opts: Options) -> Gateway {
    let ollama = fake_ollama().await;
    let api = fake_api(&[key]).await;
    let config = Config {
        port: 0,
        allowed_origin: opts.allowed_origin.clone(),
        ollama_url: format!("http://{}", ollama.addr),
        synco_api_url: format!("http://{}", api.addr),
        gateway_url: GATEWAY_URL.into(),
        allowed_orgs: opts.allowed_orgs.clone(),
        allowed_models: opts.allowed_models.clone(),
        max_body_bytes: opts.max_body_bytes,
        max_tokens: 4096,
        max_concurrent: opts.max_concurrent,
        max_concurrent_per_user: opts.max_per_user,
    };
    let keys = Arc::new(KeyStore::new(
        build_http_client().expect("client"),
        &config.synco_api_url,
        opts.keys,
    ));
    keys.refresh().await;
    let verifier = Verifier {
        gateway_url: config.gateway_url.clone(),
        allowed_orgs: config.allowed_orgs.clone(),
    };
    let state = AppState::new(&config, verifier, keys.clone()).expect("état");
    let app = router(state, config.allowed_origin.as_deref());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move { axum::serve(listener, app).await.expect("serve") });
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("client");
    Gateway {
        base: format!("http://{addr}"),
        ollama,
        api,
        keys,
        http,
    }
}

impl Gateway {
    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }
    pub async fn chat(&self, ticket: Option<&str>, body: &str) -> reqwest::Response {
        let mut req = self
            .http
            .post(self.url("/v1/chat/completions"))
            .header("content-type", "application/json")
            .body(body.to_string());
        if let Some(t) = ticket {
            req = req.header("authorization", format!("Bearer {t}"));
        }
        req.send().await.expect("réponse")
    }
}

pub const CHAT_BODY: &str =
    r#"{"model":"llama3.2","messages":[{"role":"user","content":"bonjour"}],"stream":true}"#;

/// Passerelle dont l'API de clés répond 503 dès le démarrage (jamais de clés lues).
pub async fn start_gateway_with_unreadable_api(key: &TestKey) -> Gateway {
    let ollama = fake_ollama().await;
    let api = fake_api(&[key]).await;
    api.set_raw(503, "");
    let config = Config {
        port: 0,
        allowed_origin: None,
        ollama_url: format!("http://{}", ollama.addr),
        synco_api_url: format!("http://{}", api.addr),
        gateway_url: GATEWAY_URL.into(),
        allowed_orgs: vec![ORG.to_string()],
        allowed_models: vec![],
        max_body_bytes: 64 * 1024,
        max_tokens: 4096,
        max_concurrent: 16,
        max_concurrent_per_user: 4,
    };
    let keys = Arc::new(KeyStore::new(
        build_http_client().expect("client"),
        &config.synco_api_url,
        KeyStoreConfig {
            unknown_kid_min_interval: Duration::from_secs(60),
            ..KeyStoreConfig::default()
        },
    ));
    assert!(!keys.refresh().await);
    let verifier = Verifier {
        gateway_url: config.gateway_url.clone(),
        allowed_orgs: config.allowed_orgs.clone(),
    };
    let state = AppState::new(&config, verifier, keys.clone()).expect("état");
    let app = router(state, None);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move { axum::serve(listener, app).await.expect("serve") });
    let http = reqwest::Client::new();
    Gateway {
        base: format!("http://{addr}"),
        ollama,
        api,
        keys,
        http,
    }
}
