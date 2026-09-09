use crate::ollama_adapter::{self, NormalizedEvent};
use crate::synco_client::SyncoClient;
use crate::types::{PendingToolCall, StoredMessage, StoredToolCall, WireEvent};
use async_stream::stream;
use futures_util::{Stream, StreamExt};
use serde_json::{json, Value};

/// Port de synco_api/src/services/ai/aiTurnRunner.ts. Contrairement à la version TS, il n'y a pas
/// d'abstraction TurnPersistence injectée : un seul déploiement possible ici (SyncoClient), donc
/// on appelle directement patch_session à chaque étape plutôt que de passer par une interface.
const SYSTEM_PROMPT: &str = "Tu es Synco AI, un assistant IA français, sécurisé et souverain, intégré à l'outil collaboratif Synco.
Tes réponses doivent être concises, utiles, et toujours en français.
Tu as accès à des outils réels pour agir sur l'organisation (créer une tâche, un espace, etc.) et pour consulter des informations. Utilise-les quand c'est pertinent, sans demander la permission avant de les appeler : l'utilisateur validera lui-même les actions qui le nécessitent.
Si une information te manque pour utiliser un outil correctement, demande-la à l'utilisateur plutôt que d'inventer une valeur.";

#[derive(Clone)]
pub struct TurnContext {
    pub org_id: String,
    pub session_id: String,
    pub token: String,
    pub ollama_url: String,
    pub model_id: String,
}

pub struct ResumeDecision {
    pub accepted: Option<bool>,
    pub client_result: Option<Value>,
}

fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::new_v4().simple())
}

fn now_iso() -> String {
    use time::format_description::well_known::Rfc3339;
    time::OffsetDateTime::now_utc().format(&Rfc3339).unwrap_or_default()
}

fn safe_json_parse(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|_| json!({}))
}

/// Boucle interne partagée par start_turn/resume_turn : appelle Ollama, exécute les tools
/// 'server' non-mutants immédiatement (en délégant à synco_api) puis relance un appel, et
/// s'arrête (fin du stream SSE) dès qu'un tool mutant ou 'client' doit être confirmé/exécuté
/// ailleurs — comportement identique à runLoop() côté synco_api.
fn run_loop(
    client: SyncoClient,
    http: reqwest::Client,
    ctx: TurnContext,
    mut messages: Vec<StoredMessage>,
) -> impl Stream<Item = WireEvent> {
    stream! {
        let tools = match client.get_tools_manifest(&ctx.token, &ctx.org_id).await {
            Ok(t) => t,
            Err(e) => {
                yield WireEvent::Error { error: format!("Impossible de récupérer la liste des outils: {e}") };
                return;
            }
        };

        loop {
            let mut text_buffer = String::new();
            let mut pending_call: Option<(String, String, String)> = None;
            let mut finish_reason = "stop".to_string();

            let completion = ollama_adapter::create_completion(
                http.clone(),
                ctx.ollama_url.clone(),
                ctx.model_id.clone(),
                SYSTEM_PROMPT.to_string(),
                messages.clone(),
                tools.clone(),
            );
            let mut completion = Box::pin(completion);

            while let Some(ev) = completion.next().await {
                match ev {
                    NormalizedEvent::Text { delta } => {
                        text_buffer.push_str(&delta);
                        yield WireEvent::Text { delta };
                    }
                    NormalizedEvent::ToolCall { id, name, args_json } => {
                        pending_call = Some((id, name, args_json));
                    }
                    NormalizedEvent::Done { finish_reason: fr } => {
                        finish_reason = fr;
                    }
                }
            }

            if finish_reason != "tool_calls" || pending_call.is_none() {
                messages.push(StoredMessage {
                    id: new_id("msg"),
                    role: "assistant".to_string(),
                    content: Some(text_buffer),
                    tool_calls: None,
                    tool_call_id: None,
                    tool_result: None,
                    created_at: now_iso(),
                });

                if let Err(e) = client.patch_session(&ctx.token, &ctx.org_id, &ctx.session_id, &messages, "idle", None).await {
                    yield WireEvent::Error { error: format!("Erreur de persistance: {e}") };
                    return;
                }
                yield WireEvent::Done { session_id: ctx.session_id.clone() };
                return;
            }

            let (call_id, call_name, args_json) = pending_call.unwrap();
            let args = safe_json_parse(&args_json);
            let spec = tools.iter().find(|t| t.name == call_name).cloned();

            messages.push(StoredMessage {
                id: new_id("msg"),
                role: "assistant".to_string(),
                content: if text_buffer.is_empty() { None } else { Some(text_buffer) },
                tool_calls: Some(vec![StoredToolCall {
                    id: call_id.clone(),
                    name: call_name.clone(),
                    arguments: args.clone(),
                    category: spec.as_ref().map(|s| s.category.clone()).unwrap_or_else(|| "server".to_string()),
                    mutating: spec.as_ref().map(|s| s.mutating).unwrap_or(true),
                    status: if spec.is_some() { "pending".to_string() } else { "error".to_string() },
                }]),
                tool_call_id: None,
                tool_result: None,
                created_at: now_iso(),
            });

            let Some(spec) = spec else {
                let tool_result = json!({ "error": format!("Outil inconnu: {call_name}") });
                messages.push(StoredMessage {
                    id: new_id("msg"),
                    role: "tool".to_string(),
                    content: None,
                    tool_calls: None,
                    tool_call_id: Some(call_id.clone()),
                    tool_result: Some(tool_result.clone()),
                    created_at: now_iso(),
                });
                if let Err(e) = client.patch_session(&ctx.token, &ctx.org_id, &ctx.session_id, &messages, "idle", None).await {
                    yield WireEvent::Error { error: format!("Erreur de persistance: {e}") };
                    return;
                }
                yield WireEvent::ToolCallResult { tool_call_id: call_id, name: call_name, mutating: false, result: tool_result };
                continue;
            };

            if spec.category == "server" && !spec.mutating {
                let tool_result = match client.execute_tool(&ctx.token, &ctx.org_id, &spec.name, &args).await {
                    Ok(data) => data,
                    Err(e) => json!({ "error": e.to_string() }),
                };
                messages.push(StoredMessage {
                    id: new_id("msg"),
                    role: "tool".to_string(),
                    content: None,
                    tool_calls: None,
                    tool_call_id: Some(call_id.clone()),
                    tool_result: Some(tool_result.clone()),
                    created_at: now_iso(),
                });
                if let Err(e) = client.patch_session(&ctx.token, &ctx.org_id, &ctx.session_id, &messages, "idle", None).await {
                    yield WireEvent::Error { error: format!("Erreur de persistance: {e}") };
                    return;
                }
                yield WireEvent::ToolCallResult { tool_call_id: call_id, name: call_name, mutating: false, result: tool_result };
                continue;
            }

            // Tool mutant (serveur) ou tool 'client' : on met la session en pause et on termine le stream.
            let pending = PendingToolCall {
                id: call_id.clone(),
                name: call_name.clone(),
                args: args.clone(),
                category: spec.category.clone(),
                mutating: spec.mutating,
                interactive: spec.interactive,
            };
            let status = if spec.category == "server" { "awaiting_confirmation" } else { "awaiting_client_tool" };

            if let Err(e) = client.patch_session(&ctx.token, &ctx.org_id, &ctx.session_id, &messages, status, Some(&pending)).await {
                yield WireEvent::Error { error: format!("Erreur de persistance: {e}") };
                return;
            }

            if spec.category == "server" {
                yield WireEvent::ToolCallPending { tool_call_id: call_id, name: call_name, args };
            } else {
                yield WireEvent::ToolCallClientRequired {
                    tool_call_id: call_id, name: call_name, args,
                    mutating: spec.mutating, interactive: spec.interactive,
                };
            }
            return;
        }
    }
}

pub fn start_turn(
    client: SyncoClient,
    http: reqwest::Client,
    ctx: TurnContext,
    history: Vec<StoredMessage>,
    user_text: String,
) -> impl Stream<Item = WireEvent> {
    stream! {
        let mut messages = history;
        messages.push(StoredMessage {
            id: new_id("msg"),
            role: "user".to_string(),
            content: Some(user_text),
            tool_calls: None,
            tool_call_id: None,
            tool_result: None,
            created_at: now_iso(),
        });

        if let Err(e) = client.patch_session(&ctx.token, &ctx.org_id, &ctx.session_id, &messages, "idle", None).await {
            yield WireEvent::Error { error: format!("Erreur de persistance: {e}") };
            return;
        }

        let mut inner = Box::pin(run_loop(client, http, ctx, messages));
        while let Some(ev) = inner.next().await {
            yield ev;
        }
    }
}

pub fn resume_turn(
    client: SyncoClient,
    http: reqwest::Client,
    ctx: TurnContext,
    history: Vec<StoredMessage>,
    pending: PendingToolCall,
    decision: ResumeDecision,
) -> impl Stream<Item = WireEvent> {
    stream! {
        let mut messages = history;

        let tool_result: Value = if pending.category == "client" {
            decision.client_result.unwrap_or_else(|| json!({ "error": "Aucun résultat fourni par le client." }))
        } else if decision.accepted.unwrap_or(false) {
            match client.execute_tool(&ctx.token, &ctx.org_id, &pending.name, &pending.args).await {
                Ok(data) => data,
                Err(e) => json!({ "error": e.to_string() }),
            }
        } else {
            json!({ "error": "Action refusée par l'utilisateur." })
        };

        messages.push(StoredMessage {
            id: new_id("msg"),
            role: "tool".to_string(),
            content: None,
            tool_calls: None,
            tool_call_id: Some(pending.id.clone()),
            tool_result: Some(tool_result.clone()),
            created_at: now_iso(),
        });

        if let Err(e) = client.patch_session(&ctx.token, &ctx.org_id, &ctx.session_id, &messages, "idle", None).await {
            yield WireEvent::Error { error: format!("Erreur de persistance: {e}") };
            return;
        }

        yield WireEvent::ToolCallResult {
            tool_call_id: pending.id.clone(), name: pending.name.clone(), mutating: false, result: tool_result,
        };

        let mut inner = Box::pin(run_loop(client, http, ctx, messages));
        while let Some(ev) = inner.next().await {
            yield ev;
        }
    }
}
