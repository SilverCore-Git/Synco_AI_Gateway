use crate::config::Config;
use crate::keys::KeyStore;
use crate::limits::Limiter;
use crate::relay::rewrite_body;
use crate::ticket::{peek_kid, Claims, TicketError, Verifier};
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::trace::TraceLayer;

#[derive(Clone)]
pub struct AppState {
    http: reqwest::Client,
    ollama_url: String,
    allowed_models: Arc<Vec<String>>,
    max_body_bytes: usize,
    max_tokens: u64,
    verifier: Arc<Verifier>,
    keys: Arc<KeyStore>,
    limiter: Arc<Limiter>,
}

impl AppState {
    pub fn new(config: &Config, verifier: Verifier, keys: Arc<KeyStore>) -> reqwest::Result<Self> {
        // Pas de délai global (réponses en flux) ; un silence prolongé d'Ollama coupe la requête.
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .read_timeout(Duration::from_secs(300))
            .build()?;
        Ok(Self {
            http,
            ollama_url: config.ollama_url.trim_end_matches('/').to_string(),
            allowed_models: Arc::new(config.allowed_models.clone()),
            max_body_bytes: config.max_body_bytes,
            max_tokens: config.max_tokens,
            verifier: Arc::new(verifier),
            keys,
            limiter: Limiter::new(config.max_concurrent, config.max_concurrent_per_user),
        })
    }
}

fn error(status: StatusCode, code: &str) -> Response {
    (status, Json(json!({ "error": code }))).into_response()
}

async fn not_found() -> Response {
    error(StatusCode::NOT_FOUND, "not_found")
}

async fn health() -> &'static str {
    "ok"
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Authentifie l'appel : `Authorization: Bearer <sgt1>`. Aucune valeur reçue (ticket compris)
/// n'est journalisée.
async fn authenticate(state: &AppState, headers: &HeaderMap) -> Result<Claims, TicketError> {
    let Some(value) = headers.get(header::AUTHORIZATION) else {
        return Err(TicketError::Missing);
    };
    let Some(ticket) = value.to_str().ok().and_then(|v| v.strip_prefix("Bearer ")) else {
        return Err(TicketError::Invalid);
    };
    let ticket = ticket.trim();

    // Un `kid` inconnu déclenche une relecture des clés (rotation), bornée à 1/min.
    if let Some(kid) = peek_kid(ticket) {
        if !state.keys.current().contains_key(kid) {
            state.keys.refresh_for_unknown_kid().await;
        }
    }
    let keys = state.keys.current();
    state.verifier.verify(ticket, &keys, now_secs())
}

fn unauthorized(e: TicketError) -> Response {
    error(StatusCode::UNAUTHORIZED, e.code())
}

fn bad_gateway() -> Response {
    error(StatusCode::BAD_GATEWAY, "ollama_unavailable")
}

/// GET /models — modèles présents sur Ollama, restreints à ceux que la passerelle autorise.
async fn list_models(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(e) = authenticate(&state, &headers).await {
        return unauthorized(e);
    }
    let Ok(resp) = state
        .http
        .get(format!("{}/v1/models", state.ollama_url))
        .send()
        .await
    else {
        return bad_gateway();
    };
    if !resp.status().is_success() {
        return bad_gateway();
    }
    let Ok(body) = resp.json::<Value>().await else {
        return bad_gateway();
    };
    let models: Vec<String> = body["data"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m["id"].as_str())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let models: Vec<String> = if state.allowed_models.is_empty() {
        models
    } else {
        models
            .into_iter()
            .filter(|m| state.allowed_models.contains(m))
            .collect()
    };
    Json(json!({ "models": models })).into_response()
}

/// Délai maximal pour recevoir le corps d'une requête déjà authentifiée.
const BODY_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// POST /v1/chat/completions — authentifie, limite la concurrence, plafonne et réécrit le corps,
/// relaie en flux.
///
/// La place prise dans le limiteur est tenue par ce futur puis par le flux de réponse : si le
/// client se déconnecte (avant la réponse d'Ollama ou en cours de flux), hyper abandonne le futur
/// ou le corps, la requête vers Ollama est annulée et la place est rendue.
async fn chat_completions(State(state): State<AppState>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    // L'authentification précède toute lecture du corps.
    let claims = match authenticate(&state, &parts.headers).await {
        Ok(c) => c,
        Err(e) => return unauthorized(e),
    };
    let slot = match state.limiter.try_acquire(&claims.sub) {
        Ok(s) => s,
        Err(e) => {
            let mut resp = error(StatusCode::TOO_MANY_REQUESTS, e.code());
            resp.headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
            return resp;
        }
    };

    let too_large = || error(StatusCode::PAYLOAD_TOO_LARGE, "body_too_large");
    if parts
        .headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok())
        .is_some_and(|n| n > state.max_body_bytes)
    {
        return too_large();
    }
    let raw = match tokio::time::timeout(
        BODY_READ_TIMEOUT,
        axum::body::to_bytes(body, state.max_body_bytes),
    )
    .await
    {
        Ok(Ok(raw)) => raw,
        Ok(Err(_)) => return too_large(),
        Err(_) => return error(StatusCode::REQUEST_TIMEOUT, "body_timeout"),
    };
    let forwarded = match rewrite_body(&raw, &state.allowed_models, state.max_tokens) {
        Ok(b) => b,
        Err(e) => {
            let status = if e.code() == "model_not_allowed" {
                StatusCode::FORBIDDEN
            } else {
                StatusCode::BAD_REQUEST
            };
            return error(status, e.code());
        }
    };

    // Seul `content-type` part vers Ollama : ni le ticket ni aucun autre en-tête du client.
    let upstream = state
        .http
        .post(format!("{}/v1/chat/completions", state.ollama_url))
        .header(header::CONTENT_TYPE, "application/json")
        .body(forwarded)
        .send()
        .await;
    let Ok(upstream) = upstream else {
        return bad_gateway();
    };
    if !upstream.status().is_success() {
        // Le texte d'erreur d'Ollama n'est pas relayé (code machine générique).
        return error(StatusCode::BAD_GATEWAY, "upstream_error");
    }

    let mut response = Response::builder().status(upstream.status());
    for name in [header::CONTENT_TYPE, header::CACHE_CONTROL] {
        if let Some(v) = upstream.headers().get(&name) {
            response = response.header(name, v);
        }
    }
    // La place vit autant que le flux : libérée à la fin, sur erreur ou si le client part.
    let stream = upstream.bytes_stream().map(move |chunk| {
        let _held = &slot;
        chunk
    });
    response
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| bad_gateway())
}

/// Routeur complet. `allowed_origin` : origine CORS de l'app (`*` explicite, ou aucune : pas
/// d'en-tête CORS, seul le runtime appelle).
pub fn router(state: AppState, allowed_origin: Option<&str>) -> Router {
    let mut app = Router::new()
        .route("/health", get(health).fallback(not_found))
        .route("/models", get(list_models).fallback(not_found))
        .route(
            "/v1/chat/completions",
            post(chat_completions).fallback(not_found),
        )
        .fallback(not_found)
        .with_state(state)
        // Span à méthode + chemin seulement : jamais la requête de l'URI (un ticket passé en
        // `?ticket=` ne doit pas être écrit, même en DEBUG).
        .layer(TraceLayer::new_for_http().make_span_with(|req: &Request| tracing::info_span!("request", method = %req.method(), path = %req.uri().path())));

    match allowed_origin {
        Some("*") => {
            tracing::warn!("ALLOWED_ORIGIN=* : tout site peut appeler la passerelle depuis un navigateur (un ticket reste exigé). À restreindre à l'origine de l'app.");
            app = app.layer(cors(AllowOrigin::any()));
        }
        Some(origin) => match origin.trim_end_matches('/').parse::<HeaderValue>() {
            Ok(v) => app = app.layer(cors(AllowOrigin::list([v]))),
            Err(_) => tracing::error!("ALLOWED_ORIGIN invalide : aucun en-tête CORS ne sera émis"),
        },
        None => tracing::warn!("ALLOWED_ORIGIN absent : aucun en-tête CORS (les navigateurs ne pourront pas appeler la passerelle)"),
    }
    app
}

fn cors(origin: AllowOrigin) -> CorsLayer {
    CorsLayer::new()
        .allow_origin(origin)
        .allow_methods([Method::GET, Method::POST])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE])
        .max_age(Duration::from_secs(600))
}
