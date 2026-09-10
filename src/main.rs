mod config;
mod crypto;
mod ollama_adapter;
mod routes;
mod synco_client;
mod turn_runner;
mod types;

use axum::routing::{get, post};
use axum::Router;
use config::Config;
use std::net::SocketAddr;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;

#[derive(Clone)]
pub struct AppState {
    pub http: reqwest::Client,
    pub ollama_url: String,
    pub synco_api_url: Option<String>,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    let config = Config::from_env();

    let cors = if config.allowed_origin == "*" {
        tracing::warn!(
            "ALLOWED_ORIGIN n'est pas configuré : CORS accepte actuellement N'IMPORTE QUELLE origine. \
             Ça n'expose ni la clé de session ni le token Bearer (ils vivent en mémoire JS côté \
             synco_app, jamais accessibles à une autre origine sans XSS préalable), mais retire une \
             couche de défense en profondeur. À restreindre à l'origine exacte de synco_app en \
             production (ex: ALLOWED_ORIGIN=https://app.mon-organisation.fr)."
        );
        CorsLayer::new().allow_origin(Any).allow_methods(Any).allow_headers(Any)
    } else {
        CorsLayer::new()
            .allow_origin(config.allowed_origin.parse::<axum::http::HeaderValue>().unwrap())
            .allow_methods(Any)
            .allow_headers(Any)
    };

    let state = AppState {
        http: reqwest::Client::new(),
        ollama_url: config.ollama_url.clone(),
        synco_api_url: config.synco_api_url.clone(),
    };

    let app = Router::new()
        .route("/health", get(routes::health::health))
        .route("/models", get(routes::models::list_models))
        .route("/chat", post(routes::chat::chat))
        .route("/chat/{session_id}/tool-result", post(routes::chat::tool_result))
        .with_state(state)
        .layer(cors)
        .layer(TraceLayer::new_for_http());

    let addr = SocketAddr::from(([0, 0, 0, 0], config.port));
    tracing::info!("Synco AI Gateway listening on {addr}");

    let listener = tokio::net::TcpListener::bind(addr).await.expect("failed to bind port");
    axum::serve(listener, app).await.expect("server error");
}
