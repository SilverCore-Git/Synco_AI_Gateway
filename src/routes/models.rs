use crate::AppState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;
use serde_json::{json, Value};

#[derive(Serialize)]
pub struct ModelsResponse {
    models: Vec<String>,
}

fn error_response(status: StatusCode, message: String) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

/// GET /models — liste les modèles déjà présents sur le serveur Ollama de cette passerelle
/// (state.ollama_url, une config de déploiement — cf. config.rs — pas un paramètre par requête),
/// pour peupler le sélecteur des paramètres IA côté Synco. Passe par la passerelle (plutôt qu'un
/// appel direct navigateur → Ollama) pour ne pas exiger une configuration CORS séparée sur
/// Ollama pour l'origine du frontend Synco.
pub async fn list_models(State(state): State<AppState>) -> Response {
    let url = format!("{}/v1/models", state.ollama_url.trim_end_matches('/'));

    let resp = match state.http.get(&url).send().await {
        Ok(r) => r,
        Err(e) => return error_response(StatusCode::BAD_GATEWAY, format!("Impossible de contacter Ollama: {e}")),
    };

    if !resp.status().is_success() {
        let status = resp.status();
        return error_response(StatusCode::BAD_GATEWAY, format!("Ollama a répondu {status}"));
    }

    let body: Value = match resp.json().await {
        Ok(b) => b,
        Err(e) => return error_response(StatusCode::BAD_GATEWAY, format!("Réponse Ollama invalide: {e}")),
    };

    let models: Vec<String> = body["data"]
        .as_array()
        .map(|arr| arr.iter().filter_map(|m| m["id"].as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default();

    Json(ModelsResponse { models }).into_response()
}
