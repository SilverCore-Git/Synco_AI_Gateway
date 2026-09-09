use crate::types::{PendingToolCall, StoredMessage, ToolSpec};
use reqwest::Client;
use serde_json::{json, Value};
use std::error::Error as _;

/// reqwest::Error::to_string() ne montre que le message de plus haut niveau ("error sending
/// request for url (...)") et cache la vraie cause (ex: certificat TLS invalide) dans la chaîne
/// de `.source()`. On la déroule pour avoir un message réellement diagnosticable.
fn describe_reqwest_error(e: &reqwest::Error) -> String {
    let mut msg = e.to_string();
    let mut source = e.source();
    while let Some(s) = source {
        msg.push_str(&format!(" — cause: {s}"));
        source = s.source();
    }
    msg
}

/// Client HTTP vers synco_api — la passerelle n'a et ne doit avoir aucun accès direct à la base :
/// toute la logique métier, les permissions et la persistance restent l'autorité de synco_api.
/// Chaque appel relaie tel quel le Bearer token du navigateur ; c'est synco_api qui l'authentifie
/// réellement (aucune validation JWT locale ici pour cette v1).
#[derive(Clone)]
pub struct SyncoClient {
    http: Client,
    base_url: String,
}

pub struct LoadedSession {
    pub id: String,
    pub messages: Vec<StoredMessage>,
    pub status: String,
    pub pending_tool_call: Option<PendingToolCall>,
}

#[derive(Debug)]
pub enum SyncoError {
    /// Échec réseau/désérialisation, pas une réponse d'erreur de synco_api.
    Transport(String),
    /// synco_api a répondu avec un code d'erreur.
    Api { status: u16, message: String },
}

impl std::fmt::Display for SyncoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SyncoError::Transport(e) => write!(f, "Erreur réseau vers l'API Synco: {e}"),
            SyncoError::Api { status, message } => write!(f, "L'API Synco a répondu {status}: {message}"),
        }
    }
}

impl SyncoClient {
    pub fn new(base_url: String) -> Self {
        // Développement local uniquement : synco_api tourne souvent en HTTPS avec un certificat
        // auto-signé (certs/server.crt). Le navigateur laisse l'utilisateur cliquer "continuer
        // quand même" une fois ; rustls, lui, refuse toujours un certificat non approuvé, sans
        // exception possible — d'où cette échappatoire explicite, jamais activée par défaut.
        let allow_insecure_tls = std::env::var("SYNCO_API_ALLOW_INSECURE_TLS")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);

        let http = if allow_insecure_tls {
            tracing::warn!(
                "SYNCO_API_ALLOW_INSECURE_TLS=true : les certificats TLS de l'API Synco ne sont PAS vérifiés. \
                 À utiliser uniquement en développement local, jamais en production."
            );
            Client::builder().danger_accept_invalid_certs(true).build().unwrap_or_default()
        } else {
            Client::new()
        };

        Self { http, base_url }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url.trim_end_matches('/'), path)
    }

    async fn parse_session_response(resp: reqwest::Response) -> Result<LoadedSession, SyncoError> {
        let status = resp.status();
        if !status.is_success() {
            let message = resp.text().await.unwrap_or_default();
            return Err(SyncoError::Api { status: status.as_u16(), message });
        }
        let body: Value = resp.json().await.map_err(|e| SyncoError::Transport(describe_reqwest_error(&e)))?;

        let messages: Vec<StoredMessage> = serde_json::from_value(body["messages"].clone()).unwrap_or_default();
        let pending_tool_call: Option<PendingToolCall> = if body["pendingToolCall"].is_null() {
            None
        } else {
            serde_json::from_value(body["pendingToolCall"].clone()).ok()
        };

        Ok(LoadedSession {
            id: body["id"].as_str().unwrap_or_default().to_string(),
            messages,
            status: body["status"].as_str().unwrap_or("idle").to_string(),
            pending_tool_call,
        })
    }

    pub async fn get_or_create_session(
        &self,
        token: &str,
        org_id: &str,
        session_id: Option<&str>,
    ) -> Result<LoadedSession, SyncoError> {
        match session_id {
            Some(id) => self.get_session(token, org_id, id).await,
            None => self.create_session(token, org_id).await,
        }
    }

    pub async fn get_session(&self, token: &str, org_id: &str, session_id: &str) -> Result<LoadedSession, SyncoError> {
        let url = self.url(&format!("/api/orgs/{org_id}/ai/sessions/{session_id}"));
        let resp = self.http.get(&url).bearer_auth(token).send().await.map_err(|e| SyncoError::Transport(describe_reqwest_error(&e)))?;
        Self::parse_session_response(resp).await
    }

    async fn create_session(&self, token: &str, org_id: &str) -> Result<LoadedSession, SyncoError> {
        let url = self.url(&format!("/api/orgs/{org_id}/ai/sessions"));
        let resp = self
            .http
            .post(&url)
            .bearer_auth(token)
            .json(&json!({ "title": "Nouvelle session", "messages": [] }))
            .send()
            .await
            .map_err(|e| SyncoError::Transport(describe_reqwest_error(&e)))?;
        Self::parse_session_response(resp).await
    }

    pub async fn patch_session(
        &self,
        token: &str,
        org_id: &str,
        session_id: &str,
        messages: &[StoredMessage],
        status: &str,
        pending_tool_call: Option<&PendingToolCall>,
    ) -> Result<(), SyncoError> {
        let url = self.url(&format!("/api/orgs/{org_id}/ai/sessions/{session_id}"));
        let resp = self
            .http
            .patch(&url)
            .bearer_auth(token)
            .json(&json!({
                "messages": messages,
                "status": status,
                "pendingToolCall": pending_tool_call,
            }))
            .send()
            .await
            .map_err(|e| SyncoError::Transport(describe_reqwest_error(&e)))?;

        let status_code = resp.status();
        if !status_code.is_success() {
            let message = resp.text().await.unwrap_or_default();
            return Err(SyncoError::Api { status: status_code.as_u16(), message });
        }
        Ok(())
    }

    pub async fn get_tools_manifest(&self, token: &str, org_id: &str) -> Result<Vec<ToolSpec>, SyncoError> {
        let url = self.url(&format!("/api/orgs/{org_id}/ai/tools"));
        let resp = self.http.get(&url).bearer_auth(token).send().await.map_err(|e| SyncoError::Transport(describe_reqwest_error(&e)))?;

        let status = resp.status();
        if !status.is_success() {
            let message = resp.text().await.unwrap_or_default();
            return Err(SyncoError::Api { status: status.as_u16(), message });
        }

        resp.json::<Vec<ToolSpec>>().await.map_err(|e| SyncoError::Transport(describe_reqwest_error(&e)))
    }

    /// Exécute un tool 'server' via POST /api/orgs/:orgId/ai/tools/:name/execute — toute la
    /// logique métier/permissions vit dans synco_api, cette passerelle ne fait que relayer.
    pub async fn execute_tool(&self, token: &str, org_id: &str, name: &str, args: &Value) -> Result<Value, SyncoError> {
        let url = self.url(&format!("/api/orgs/{org_id}/ai/tools/{name}/execute"));
        let resp = self
            .http
            .post(&url)
            .bearer_auth(token)
            .json(&json!({ "args": args }))
            .send()
            .await
            .map_err(|e| SyncoError::Transport(describe_reqwest_error(&e)))?;

        let status = resp.status();
        let body: Value = resp.json().await.unwrap_or(Value::Null);

        if !status.is_success() {
            let message = body["error"].as_str().unwrap_or("Erreur inconnue").to_string();
            return Err(SyncoError::Api { status: status.as_u16(), message });
        }

        Ok(body["data"].clone())
    }
}
