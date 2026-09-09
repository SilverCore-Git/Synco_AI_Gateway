use crate::synco_client::SyncoClient;
use crate::turn_runner::{self, ResumeDecision, TurnContext};
use crate::types::WireEvent;
use crate::AppState;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures_util::{Stream, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use std::convert::Infallible;

fn extract_bearer(headers: &HeaderMap) -> Option<String> {
    let value = headers.get("authorization")?.to_str().ok()?;
    value.strip_prefix("Bearer ").map(|s| s.to_string())
}

fn error_response(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

fn wire_event_to_sse(ev: WireEvent) -> Result<Event, Infallible> {
    Ok(Event::default().data(serde_json::to_string(&ev).unwrap_or_default()))
}

fn sse_response(stream: impl Stream<Item = WireEvent> + Send + 'static) -> Response {
    Sse::new(stream.map(wire_event_to_sse)).into_response()
}

/// Champs envoyés par le frontend à chaque appel — la passerelle n'a aucune config par
/// organisation, l'URL de l'API Synco et le modèle viennent d'ici, comme configuré dans les
/// paramètres IA de l'org (provider "gateway"). L'URL d'Ollama, elle, est une config de
/// déploiement de la passerelle (state.ollama_url, cf. config.rs) — pas un champ par requête.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatRequest {
    pub org_id: String,
    pub session_id: Option<String>,
    pub message: String,
    pub synco_api_url: String,
    pub model_id: String,
}

// POST /chat — démarre ou continue une conversation
pub async fn chat(State(state): State<AppState>, headers: HeaderMap, Json(req): Json<ChatRequest>) -> Response {
    let Some(token) = extract_bearer(&headers) else {
        return error_response(StatusCode::UNAUTHORIZED, "Authorization Bearer token manquant.");
    };

    let client = SyncoClient::new(req.synco_api_url);

    let session = match client.get_or_create_session(&token, &req.org_id, req.session_id.as_deref()).await {
        Ok(s) => s,
        Err(e) => return error_response(StatusCode::BAD_GATEWAY, &e.to_string()),
    };

    let ctx = TurnContext {
        org_id: req.org_id,
        session_id: session.id.clone(),
        token,
        ollama_url: state.ollama_url.clone(),
        model_id: req.model_id,
    };

    let session_event = futures_util::stream::once({
        let session_id = session.id.clone();
        async move { WireEvent::Session { session_id } }
    });

    let body = turn_runner::start_turn(client, state.http, ctx, session.messages, req.message);

    sse_response(session_event.chain(body))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResultRequest {
    pub org_id: String,
    pub synco_api_url: String,
    pub model_id: String,
    #[serde(default)]
    pub accepted: Option<bool>,
    #[serde(default)]
    pub client_result: Option<Value>,
}

// POST /chat/:session_id/tool-result — reprend après confirmation/exécution client
pub async fn tool_result(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    headers: HeaderMap,
    Json(req): Json<ToolResultRequest>,
) -> Response {
    let Some(token) = extract_bearer(&headers) else {
        return error_response(StatusCode::UNAUTHORIZED, "Authorization Bearer token manquant.");
    };

    let client = SyncoClient::new(req.synco_api_url);

    let session = match client.get_session(&token, &req.org_id, &session_id).await {
        Ok(s) => s,
        Err(e) => return error_response(StatusCode::BAD_GATEWAY, &e.to_string()),
    };

    if session.status == "idle" {
        return error_response(StatusCode::CONFLICT, "Cette session n'attend pas de résultat de tool.");
    }
    let Some(pending) = session.pending_tool_call.clone() else {
        return error_response(StatusCode::CONFLICT, "Cette session n'attend pas de résultat de tool.");
    };

    let ctx = TurnContext {
        org_id: req.org_id,
        session_id: session.id.clone(),
        token,
        ollama_url: state.ollama_url.clone(),
        model_id: req.model_id,
    };

    let decision = ResumeDecision { accepted: req.accepted, client_result: req.client_result };
    let body = turn_runner::resume_turn(client, state.http, ctx, session.messages, pending, decision);

    sse_response(body)
}
