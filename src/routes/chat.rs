use crate::crypto::{self, SessionKey};
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

/// Extrait et valide le header `X-Session-Key` (E2EE_PLAN.md §3). Fail-closed : absent ou de
/// mauvaise longueur → toujours un 400, jamais un retour silencieux vers un mode "session en
/// clair".
fn extract_session_key(headers: &HeaderMap) -> Result<SessionKey, Response> {
    let invalid = || error_response(StatusCode::BAD_REQUEST, "Header X-Session-Key manquant ou invalide.");
    let raw = headers.get("x-session-key").and_then(|v| v.to_str().ok()).ok_or_else(invalid)?;
    crypto::parse_session_key_header(raw).map_err(|_| invalid())
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

/// L'URL de l'API Synco vue par le navigateur (VITE_API_URL) et celle vue par la passerelle ne
/// sont pas forcément la même adresse — typiquement, si la passerelle tourne dans un conteneur
/// Docker et que synco_api tourne sur la machine hôte, "localhost" ne désigne pas la même
/// machine des deux côtés. SYNCO_API_URL (config de déploiement de la passerelle, cf. config.rs)
/// a donc toujours priorité sur ce que le frontend envoie ; ce dernier ne reste qu'un filet de
/// sécurité pour les déploiements qui n'ont pas configuré cette variable.
fn resolve_synco_api_url(state: &AppState, from_request: Option<String>) -> Result<String, Response> {
    state.synco_api_url.clone().or(from_request).ok_or_else(|| {
        error_response(
            StatusCode::BAD_REQUEST,
            "URL de l'API Synco inconnue : configurez SYNCO_API_URL sur la passerelle (recommandé, surtout si elle tourne dans un conteneur), ou envoyez syncoApiUrl depuis le frontend.",
        )
    })
}

/// Champs envoyés par le frontend à chaque appel — la passerelle n'a aucune config par
/// organisation, le modèle vient d'ici, comme configuré dans les paramètres IA de l'org
/// (provider "gateway"). L'URL d'Ollama et l'URL de l'API Synco sont normalement des configs de
/// déploiement de la passerelle (state.ollama_url / state.synco_api_url, cf. config.rs) —
/// synco_api_url reste acceptée ici en filet de sécurité, voir resolve_synco_api_url().
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatRequest {
    pub org_id: String,
    pub session_id: Option<String>,
    pub message: String,
    #[serde(default)]
    pub synco_api_url: Option<String>,
    pub model_id: String,
}

// POST /chat — démarre ou continue une conversation
pub async fn chat(State(state): State<AppState>, headers: HeaderMap, Json(req): Json<ChatRequest>) -> Response {
    let Some(token) = extract_bearer(&headers) else {
        return error_response(StatusCode::UNAUTHORIZED, "Authorization Bearer token manquant.");
    };
    let session_key = match extract_session_key(&headers) {
        Ok(k) => k,
        Err(resp) => return resp,
    };

    let synco_api_url = match resolve_synco_api_url(&state, req.synco_api_url) {
        Ok(url) => url,
        Err(resp) => return resp,
    };
    let client = SyncoClient::new(synco_api_url, session_key);

    // get_or_create_session (lit/crée LA session) et get_tools_manifest (lit le manifeste des
    // tools de l'org) sont deux appels synco_api totalement indépendants l'un de l'autre — les
    // paralléliser économise un aller-retour réseau complet avant même le premier appel à Ollama.
    let (session_result, tools_result) = tokio::join!(
        client.get_or_create_session(&token, &req.org_id, req.session_id.as_deref()),
        client.get_tools_manifest(&token, &req.org_id),
    );

    let session = match session_result {
        Ok(s) => s,
        Err(e) => return error_response(StatusCode::BAD_GATEWAY, &e.to_string()),
    };
    let tools = match tools_result {
        Ok(t) => t,
        Err(e) => return error_response(StatusCode::BAD_GATEWAY, &format!("Impossible de récupérer la liste des outils: {e}")),
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

    let body = turn_runner::start_turn(client, state.http, ctx, session.messages, req.message, tools);

    sse_response(session_event.chain(body))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResultRequest {
    pub org_id: String,
    #[serde(default)]
    pub synco_api_url: Option<String>,
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
    let session_key = match extract_session_key(&headers) {
        Ok(k) => k,
        Err(resp) => return resp,
    };

    let synco_api_url = match resolve_synco_api_url(&state, req.synco_api_url) {
        Ok(url) => url,
        Err(resp) => return resp,
    };
    let client = SyncoClient::new(synco_api_url, session_key);

    // Même parallélisation que chat() : get_session et get_tools_manifest sont indépendants.
    let (session_result, tools_result) = tokio::join!(
        client.get_session(&token, &req.org_id, &session_id),
        client.get_tools_manifest(&token, &req.org_id),
    );

    let session = match session_result {
        Ok(s) => s,
        Err(e) => return error_response(StatusCode::BAD_GATEWAY, &e.to_string()),
    };
    let tools = match tools_result {
        Ok(t) => t,
        Err(e) => return error_response(StatusCode::BAD_GATEWAY, &format!("Impossible de récupérer la liste des outils: {e}")),
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
    let body = turn_runner::resume_turn(client, state.http, ctx, session.messages, pending, decision, tools);

    sse_response(body)
}
