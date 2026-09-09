mod config;

use axum::{routing::get, Router};
use config::Config;
use std::net::SocketAddr;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;

async fn health() -> &'static str {
    "ok"
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    let config = Config::from_env();

    let cors = if config.allowed_origin == "*" {
        CorsLayer::new().allow_origin(Any).allow_methods(Any).allow_headers(Any)
    } else {
        CorsLayer::new()
            .allow_origin(config.allowed_origin.parse::<axum::http::HeaderValue>().unwrap())
            .allow_methods(Any)
            .allow_headers(Any)
    };

    let app = Router::new()
        .route("/health", get(health))
        .layer(cors)
        .layer(TraceLayer::new_for_http());

    let addr = SocketAddr::from(([0, 0, 0, 0], config.port));
    tracing::info!("Synco AI Gateway listening on {addr}");

    let listener = tokio::net::TcpListener::bind(addr).await.expect("failed to bind port");
    axum::serve(listener, app).await.expect("server error");
}
