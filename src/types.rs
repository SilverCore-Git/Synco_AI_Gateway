use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Miroir de synco_api/src/services/ai/types.ts — même forme JSON exacte, pour que le frontend
/// (AgentTurn.vue, ToolStepItem.vue, consumeAgentStream) n'ait rien à changer selon que la boucle
/// tourne côté synco_api ou côté cette passerelle.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
    /// "server" | "client"
    pub category: String,
    pub mutating: bool,
    /// "pending" | "accepted" | "rejected" | "executing" | "done" | "error"
    pub status: String,
}

/// Miroir de UsageInfo côté synco_api (providerAdapters/types.ts) — comptages agrégés,
/// non sensibles (contrairement à `thinking`), donc jamais chiffrés dans synco_client.rs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredMessage {
    pub id: String,
    /// "user" | "assistant" | "tool"
    pub role: String,
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<StoredToolCall>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_result: Option<Value>,
    /// Raisonnement accumulé du modèle pour ce message, quand le fournisseur l'expose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<UsageInfo>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingToolCall {
    pub id: String,
    pub name: String,
    pub args: Value,
    /// "server" | "client"
    pub category: String,
    pub mutating: bool,
    pub interactive: bool,
}

/// Une entrée du manifeste renvoyé par GET /api/orgs/:orgId/ai/tools côté synco_api — c'est la
/// même decision que celle prise par AiTurnRunner: category='server' + mutating=false → auto-exec,
/// category='server' + mutating=true → pause pour confirmation, category='client' → toujours géré
/// par le frontend, jamais par cette passerelle.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: Value,
    pub category: String,
    pub mutating: bool,
    #[serde(default)]
    pub interactive: bool,
}

/// Événements envoyés au frontend en SSE (une ligne `data: <json>` par événement) — noms de
/// variantes et de champs choisis explicitement pour matcher au caractère près le WireEvent de
/// synco_api/src/services/ai/types.ts, plutôt que de compter sur rename_all pour un enum taggé.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum WireEvent {
    #[serde(rename = "session")]
    Session {
        #[serde(rename = "sessionId")]
        session_id: String,
    },
    #[serde(rename = "text")]
    Text { delta: String },
    #[serde(rename = "thinking")]
    Thinking { delta: String },
    #[serde(rename = "usage")]
    Usage {
        #[serde(flatten)]
        info: UsageInfo,
    },
    #[serde(rename = "tool_call_result")]
    ToolCallResult {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        name: String,
        mutating: bool,
        result: Value,
    },
    #[serde(rename = "tool_call_pending")]
    ToolCallPending {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        name: String,
        args: Value,
    },
    #[serde(rename = "tool_call_client_required")]
    ToolCallClientRequired {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        name: String,
        args: Value,
        mutating: bool,
        interactive: bool,
    },
    #[serde(rename = "done")]
    Done {
        #[serde(rename = "sessionId")]
        session_id: String,
    },
    #[serde(rename = "error")]
    Error { error: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn wire_event_json_shape_matches_ts() {
        let ev = WireEvent::ToolCallClientRequired {
            tool_call_id: "abc".into(),
            name: "search_messages".into(),
            args: json!({"query": "test"}),
            mutating: false,
            interactive: false,
        };
        let v = serde_json::to_value(&ev).unwrap();
        assert_eq!(v["type"], "tool_call_client_required");
        assert_eq!(v["toolCallId"], "abc");
        assert_eq!(v["name"], "search_messages");
        assert_eq!(v["mutating"], false);
        assert_eq!(v["interactive"], false);

        let done = WireEvent::Done { session_id: "s1".into() };
        let v2 = serde_json::to_value(&done).unwrap();
        assert_eq!(v2["type"], "done");
        assert_eq!(v2["sessionId"], "s1");
    }

    /// `#[serde(flatten)]` sur un enum à tag interne : vérifie que les champs d'UsageInfo
    /// ressortent bien à plat, au même niveau que "type" — comme `{type:'usage'} & UsageInfo`
    /// côté TS — et pas nichés sous une clé "info".
    #[test]
    fn usage_event_is_flattened_like_ts() {
        let ev = WireEvent::Usage {
            info: UsageInfo {
                prompt_tokens: Some(10),
                completion_tokens: Some(20),
                total_tokens: Some(30),
                reasoning_tokens: None,
            },
        };
        let v = serde_json::to_value(&ev).unwrap();
        assert_eq!(v["type"], "usage");
        assert_eq!(v["promptTokens"], 10);
        assert_eq!(v["completionTokens"], 20);
        assert_eq!(v["totalTokens"], 30);
        assert!(v.get("reasoningTokens").is_none(), "reasoningTokens absent doit être omis, pas null");
        assert!(v.get("info").is_none(), "les champs d'UsageInfo doivent être à plat, pas nichés");
    }

    #[test]
    fn thinking_event_json_shape_matches_ts() {
        let ev = WireEvent::Thinking { delta: "je réfléchis...".into() };
        let v = serde_json::to_value(&ev).unwrap();
        assert_eq!(v["type"], "thinking");
        assert_eq!(v["delta"], "je réfléchis...");
    }

    #[test]
    fn stored_message_thinking_and_usage_roundtrip() {
        let raw = json!({
            "id": "m1",
            "role": "assistant",
            "content": "hi",
            "thinking": "raisonnement interne",
            "usage": { "promptTokens": 5, "completionTokens": 7, "totalTokens": 12 },
            "createdAt": "2026-01-01T00:00:00Z"
        });
        let msg: StoredMessage = serde_json::from_value(raw).unwrap();
        assert_eq!(msg.thinking.as_deref(), Some("raisonnement interne"));
        let usage = msg.usage.unwrap();
        assert_eq!(usage.prompt_tokens, Some(5));
        assert_eq!(usage.total_tokens, Some(12));
        assert_eq!(usage.reasoning_tokens, None);
    }

    #[test]
    fn stored_message_camel_case_roundtrip() {
        let raw = json!({
            "id": "m1",
            "role": "assistant",
            "content": "hi",
            "toolCalls": [{
                "id": "c1", "name": "create_task", "arguments": {"title":"x"},
                "category": "server", "mutating": true, "status": "pending"
            }],
            "createdAt": "2026-01-01T00:00:00Z"
        });
        let msg: StoredMessage = serde_json::from_value(raw).unwrap();
        assert_eq!(msg.tool_calls.unwrap()[0].name, "create_task");
        assert_eq!(msg.created_at, "2026-01-01T00:00:00Z");
    }
}
