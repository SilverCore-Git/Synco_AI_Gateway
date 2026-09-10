use crate::crypto::{self, SessionKey};
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
    /// Clé de la session IA courante — reçue via le header `X-Session-Key` à chaque requête
    /// entrante, jamais persistée au-delà de la durée de vie de ce client construit par requête
    /// (E2EE_PLAN.md §3/§4). Utilisée pour (dé)chiffrer `content`/`tool_result`/`arguments` aux
    /// frontières I/O avec synco_api — voir `decrypt_loaded_session`/`encrypt_messages_for_wire`.
    session_key: SessionKey,
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
    pub fn new(base_url: String, session_key: SessionKey) -> Self {
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

        Self { http, base_url, session_key }
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

    /// Déchiffre en place `content`/`tool_result`/`tool_calls[].arguments`/`pending_tool_call.args`
    /// (E2EE_PLAN.md §4, section `synco_client.rs`). Les champs déjà en clair (sessions écrites
    /// avant activation du chiffrement) passent au travers inchangés, voir
    /// `crypto::decrypt_field_if_encrypted`.
    fn decrypt_loaded_session(&self, mut session: LoadedSession) -> Result<LoadedSession, SyncoError> {
        let session_id = session.id.clone();

        for msg in &mut session.messages {
            if let Some(content) = msg.content.take() {
                let decrypted = crypto::decrypt_field_if_encrypted(&self.session_key, &session_id, &msg.id, "content", &content)
                    .map_err(|e| SyncoError::Transport(format!("Déchiffrement du contenu du message échoué: {e}")))?;
                msg.content = Some(decrypted);
            }

            if let Some(tool_result) = msg.tool_result.take() {
                let decrypted = crypto::decrypt_json_field_if_encrypted(&self.session_key, &session_id, &msg.id, "tool_result", tool_result)
                    .map_err(|e| SyncoError::Transport(format!("Déchiffrement du résultat d'outil échoué: {e}")))?;
                msg.tool_result = Some(decrypted);
            }

            if let Some(tool_calls) = &mut msg.tool_calls {
                for tc in tool_calls.iter_mut() {
                    let field_name = format!("tool_call_arguments:{}", tc.id);
                    tc.arguments = crypto::decrypt_json_field_if_encrypted(&self.session_key, &session_id, &msg.id, &field_name, tc.arguments.clone())
                        .map_err(|e| SyncoError::Transport(format!("Déchiffrement des arguments d'outil échoué: {e}")))?;
                }
            }
        }

        if let Some(pending) = &mut session.pending_tool_call {
            pending.args = crypto::decrypt_json_field_if_encrypted(
                &self.session_key,
                &session_id,
                "__pending__",
                "pending_tool_call_args",
                pending.args.clone(),
            )
            .map_err(|e| SyncoError::Transport(format!("Déchiffrement de l'appel d'outil en attente échoué: {e}")))?;
        }

        Ok(session)
    }

    /// Chiffre une copie de `messages` pour l'envoi PATCH (E2EE_PLAN.md §4). `id`, `role`,
    /// `tool_call_id`, `created_at` restent en clair — nécessaires à synco_api/au frontend sans
    /// déchiffrement.
    fn encrypt_messages_for_wire(&self, session_id: &str, messages: &[StoredMessage]) -> Vec<StoredMessage> {
        messages
            .iter()
            .cloned()
            .map(|mut m| {
                if let Some(content) = &m.content {
                    m.content = Some(crypto::encrypt_field(&self.session_key, session_id, &m.id, "content", content));
                }
                if let Some(tool_result) = &m.tool_result {
                    m.tool_result = Some(Value::String(crypto::encrypt_json_field(&self.session_key, session_id, &m.id, "tool_result", tool_result)));
                }
                if let Some(tool_calls) = &mut m.tool_calls {
                    for tc in tool_calls.iter_mut() {
                        let field_name = format!("tool_call_arguments:{}", tc.id);
                        tc.arguments = Value::String(crypto::encrypt_json_field(&self.session_key, session_id, &m.id, &field_name, &tc.arguments));
                    }
                }
                m
            })
            .collect()
    }

    /// Chiffre une copie de `pending` pour l'envoi PATCH ; `id`, `name`, `category`, `mutating`,
    /// `interactive` restent en clair, seul `args` est sensible.
    fn encrypt_pending_for_wire(&self, session_id: &str, pending: &PendingToolCall) -> PendingToolCall {
        let mut p = pending.clone();
        p.args = Value::String(crypto::encrypt_json_field(&self.session_key, session_id, "__pending__", "pending_tool_call_args", &pending.args));
        p
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
        let session = Self::parse_session_response(resp).await?;
        self.decrypt_loaded_session(session)
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
        let session = Self::parse_session_response(resp).await?;
        self.decrypt_loaded_session(session)
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
        let encrypted_messages = self.encrypt_messages_for_wire(session_id, messages);
        let encrypted_pending = pending_tool_call.map(|p| self.encrypt_pending_for_wire(session_id, p));
        let resp = self
            .http
            .patch(&url)
            .bearer_auth(token)
            .json(&json!({
                "messages": encrypted_messages,
                "status": status,
                "pendingToolCall": encrypted_pending,
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
