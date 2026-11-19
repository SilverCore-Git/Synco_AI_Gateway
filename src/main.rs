use std::net::SocketAddr;
use std::sync::Arc;
use synco_ai_gateway::app::{router, AppState};
use synco_ai_gateway::config::Config;
use synco_ai_gateway::keys::{build_http_client, KeyStore, KeyStoreConfig};
use synco_ai_gateway::ticket::Verifier;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    if let Err(e) = run().await {
        tracing::error!("démarrage impossible : {e}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let config = Config::from_env()?;

    let key_client = build_http_client().map_err(|e| format!("client HTTP: {e}"))?;
    let keys = Arc::new(KeyStore::new(
        key_client,
        &config.synco_api_url,
        KeyStoreConfig::default(),
    ));
    if !keys.refresh().await {
        // Échec fermé : la passerelle démarre mais refuse tout ticket tant que les clés sont
        // illisibles ; une relecture est tentée sur chaque `kid` inconnu (≤ 1/min) et toutes les 10 min.
        tracing::warn!("clés de tickets illisibles au démarrage : tous les appels authentifiés seront refusés tant que l'API est injoignable");
    }
    keys.spawn_refresh_loop();

    let verifier = Verifier {
        gateway_url: config.gateway_url.clone(),
        allowed_orgs: config.allowed_orgs.clone(),
    };
    let state = AppState::new(&config, verifier, keys).map_err(|e| format!("client HTTP: {e}"))?;
    let app = router(state, config.allowed_origin.as_deref());

    let addr = SocketAddr::from(([0, 0, 0, 0], config.port));
    tracing::info!("Synco AI Gateway à l'écoute sur {addr}");
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| format!("port {}: {e}", config.port))?;
    axum::serve(listener, app)
        .await
        .map_err(|e| format!("serveur: {e}"))
}
