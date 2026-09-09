use crate::types::{StoredMessage, ToolSpec};
use async_stream::stream;
use futures_util::{Stream, StreamExt};
use serde_json::{json, Value};

/// Miroir de synco_api/src/services/ai/providerAdapters/types.ts::NormalizedEvent — usage
/// interne uniquement (jamais sérialisé tel quel), le turn runner le traduit en WireEvent.
pub enum NormalizedEvent {
    Text { delta: String },
    ToolCall { id: String, name: String, args_json: String },
    Done { finish_reason: String },
}

/// Équivalent de toWireMessages() dans providerAdapters/openai.ts. Ollama expose un endpoint
/// compatible OpenAI (/v1/chat/completions, tools inclus pour les modèles qui les supportent),
/// donc le même mapping s'applique tel quel.
fn to_wire_messages(system_prompt: &str, messages: &[StoredMessage]) -> Vec<Value> {
    let mut wire = vec![json!({ "role": "system", "content": system_prompt })];

    for m in messages {
        if m.role == "tool" {
            let content = match &m.tool_result {
                Some(Value::String(s)) => s.clone(),
                Some(v) => v.to_string(),
                None => "null".to_string(),
            };
            wire.push(json!({
                "role": "tool",
                "tool_call_id": m.tool_call_id,
                "content": content,
            }));
            continue;
        }

        if m.role == "assistant" {
            if let Some(tool_calls) = &m.tool_calls {
                if !tool_calls.is_empty() {
                    let tc_json: Vec<Value> = tool_calls
                        .iter()
                        .map(|tc| {
                            json!({
                                "id": tc.id,
                                "type": "function",
                                "function": {
                                    "name": tc.name,
                                    "arguments": serde_json::to_string(&tc.arguments).unwrap_or_else(|_| "{}".to_string()),
                                }
                            })
                        })
                        .collect();
                    wire.push(json!({
                        "role": "assistant",
                        "content": m.content,
                        "tool_calls": tc_json,
                    }));
                    continue;
                }
            }
        }

        wire.push(json!({ "role": m.role, "content": m.content }));
    }

    wire
}

/// Un seul tool_call par tour, comme providerAdapters/openai.ts (parallel_tool_calls désactivé) —
/// simplifie la machine à états et Ollama ne garantit pas le tool-calling parallèle de toute façon.
pub fn create_completion(
    client: reqwest::Client,
    ollama_url: String,
    model_id: String,
    system_prompt: String,
    messages: Vec<StoredMessage>,
    tools: Vec<ToolSpec>,
) -> impl Stream<Item = NormalizedEvent> {
    stream! {
        let mut body = json!({
            "model": model_id,
            "messages": to_wire_messages(&system_prompt, &messages),
            "stream": true,
        });

        if !tools.is_empty() {
            let tools_json: Vec<Value> = tools
                .iter()
                .map(|t| {
                    json!({
                        "type": "function",
                        "function": { "name": t.name, "description": t.description, "parameters": t.parameters }
                    })
                })
                .collect();
            body["tools"] = json!(tools_json);
            body["tool_choice"] = json!("auto");
        }

        let url = format!("{}/v1/chat/completions", ollama_url.trim_end_matches('/'));

        let resp = match client.post(&url).json(&body).send().await {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("Erreur d'appel à Ollama ({url}): {e}");
                yield NormalizedEvent::Done { finish_reason: format!("error: {e}") };
                return;
            }
        };

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            tracing::error!("Ollama a répondu {status}: {text}");
            yield NormalizedEvent::Done { finish_reason: format!("error: {status}") };
            return;
        }

        let mut byte_stream = resp.bytes_stream();
        let mut buffer = String::new();

        let mut tool_call_id: Option<String> = None;
        let mut tool_call_name: Option<String> = None;
        let mut tool_call_args = String::new();
        let mut finish_reason = "stop".to_string();

        while let Some(chunk) = byte_stream.next().await {
            let chunk = match chunk {
                Ok(c) => c,
                Err(_) => break,
            };
            buffer.push_str(&String::from_utf8_lossy(&chunk));

            while let Some(pos) = buffer.find('\n') {
                let line = buffer[..pos].to_string();
                buffer.drain(..=pos);

                let trimmed = line.trim();
                if !trimmed.starts_with("data:") {
                    continue;
                }
                let payload = trimmed["data:".len()..].trim();
                if payload.is_empty() || payload == "[DONE]" {
                    continue;
                }

                let json: Value = match serde_json::from_str(payload) {
                    Ok(j) => j,
                    Err(_) => continue,
                };

                let choice = &json["choices"][0];
                let delta = &choice["delta"];

                if let Some(content) = delta["content"].as_str() {
                    if !content.is_empty() {
                        yield NormalizedEvent::Text { delta: content.to_string() };
                    }
                }

                if let Some(tc) = delta["tool_calls"].get(0) {
                    if let Some(id) = tc["id"].as_str() {
                        tool_call_id = Some(id.to_string());
                    }
                    if let Some(name) = tc["function"]["name"].as_str() {
                        tool_call_name = Some(name.to_string());
                    }
                    if let Some(args) = tc["function"]["arguments"].as_str() {
                        tool_call_args.push_str(args);
                    }
                }

                if let Some(fr) = choice["finish_reason"].as_str() {
                    finish_reason = fr.to_string();
                }
            }
        }

        if finish_reason == "tool_calls" {
            if let (Some(id), Some(name)) = (tool_call_id, tool_call_name) {
                let args_json = if tool_call_args.is_empty() { "{}".to_string() } else { tool_call_args };
                yield NormalizedEvent::ToolCall { id, name, args_json };
            }
        }

        yield NormalizedEvent::Done { finish_reason };
    }
}
