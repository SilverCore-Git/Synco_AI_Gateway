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

/// "idle" | "awaiting_confirmation" | "awaiting_client_tool"
pub type SessionStatus = String;

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
