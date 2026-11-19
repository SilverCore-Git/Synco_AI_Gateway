//! Réécriture du corps de `POST /v1/chat/completions` (même rôle que `synco_api/src/routes/aiProxy.ts`).
//!
//! Le client ne choisit ni les options d'Ollama ni le modèle librement : seuls les champs de la
//! liste blanche sont relayés, le modèle doit être autorisé et `max_tokens` est plafonné.

use serde_json::{json, Map, Value};

/// Champs OpenAI-compatibles relayés à Ollama ; tout le reste est supprimé.
const ALLOWED_FIELDS: [&str; 16] = [
    "model",
    "messages",
    "tools",
    "tool_choice",
    "stream",
    "stream_options",
    "temperature",
    "top_p",
    "stop",
    "seed",
    "presence_penalty",
    "frequency_penalty",
    "response_format",
    "reasoning_effort",
    "reasoning",
    "max_completion_tokens",
];

#[derive(Debug, PartialEq, Eq)]
pub enum RewriteError {
    InvalidJson,
    InvalidBody,
    ModelNotAllowed,
}

impl RewriteError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidJson => "invalid_json",
            Self::InvalidBody => "invalid_body",
            Self::ModelNotAllowed => "model_not_allowed",
        }
    }
}

fn cap(value: Option<&Value>, max: u64) -> u64 {
    match value.and_then(Value::as_u64) {
        Some(n) if n > 0 => n.min(max),
        _ => max,
    }
}

/// Renvoie le corps à envoyer à Ollama. `allowed_models` vide = tout modèle (chaîne non vide).
pub fn rewrite_body(
    raw: &[u8],
    allowed_models: &[String],
    max_tokens: u64,
) -> Result<Vec<u8>, RewriteError> {
    let parsed: Value = serde_json::from_slice(raw).map_err(|_| RewriteError::InvalidJson)?;
    let Value::Object(client) = parsed else {
        return Err(RewriteError::InvalidBody);
    };

    let model = client
        .get("model")
        .and_then(Value::as_str)
        .filter(|m| !m.is_empty())
        .ok_or(RewriteError::InvalidBody)?;
    if !allowed_models.is_empty() && !allowed_models.iter().any(|m| m == model) {
        return Err(RewriteError::ModelNotAllowed);
    }
    if !client.get("messages").is_some_and(Value::is_array) {
        return Err(RewriteError::InvalidBody);
    }

    let mut out: Map<String, Value> = client
        .iter()
        .filter(|(k, _)| ALLOWED_FIELDS.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    out.insert(
        "max_tokens".into(),
        json!(cap(client.get("max_tokens"), max_tokens)),
    );
    if out.contains_key("max_completion_tokens") {
        out.insert(
            "max_completion_tokens".into(),
            json!(cap(client.get("max_completion_tokens"), max_tokens)),
        );
    }
    out.insert("n".into(), json!(1));
    serde_json::to_vec(&Value::Object(out)).map_err(|_| RewriteError::InvalidBody)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rewrite(body: Value, models: &[&str]) -> Result<Value, RewriteError> {
        let models: Vec<String> = models.iter().map(|s| s.to_string()).collect();
        rewrite_body(body.to_string().as_bytes(), &models, 4096)
            .map(|b| serde_json::from_slice(&b).unwrap())
    }

    #[test]
    fn plafonne_max_tokens_et_force_n() {
        let out = rewrite(
            json!({"model":"m","messages":[],"max_tokens":999999,"n":50}),
            &["m"],
        )
        .unwrap();
        assert_eq!(out["max_tokens"], 4096);
        assert_eq!(out["n"], 1);
    }

    #[test]
    fn max_tokens_absent_ou_invalide_prend_le_plafond() {
        for v in [
            json!({"model":"m","messages":[]}),
            json!({"model":"m","messages":[],"max_tokens":-5}),
            json!({"model":"m","messages":[],"max_tokens":"x"}),
            json!({"model":"m","messages":[],"max_tokens":0}),
        ] {
            assert_eq!(rewrite(v, &[]).unwrap()["max_tokens"], 4096);
        }
        assert_eq!(
            rewrite(json!({"model":"m","messages":[],"max_tokens":100}), &[]).unwrap()
                ["max_tokens"],
            100
        );
    }

    #[test]
    fn plafonne_max_completion_tokens() {
        let out = rewrite(
            json!({"model":"m","messages":[],"max_completion_tokens":1e9}),
            &[],
        )
        .unwrap();
        assert_eq!(out["max_completion_tokens"], 4096);
    }

    #[test]
    fn refuse_un_modele_non_autorise() {
        assert_eq!(
            rewrite(json!({"model":"autre","messages":[]}), &["m"]),
            Err(RewriteError::ModelNotAllowed)
        );
        assert!(rewrite(json!({"model":"m","messages":[]}), &["m"]).is_ok());
    }

    #[test]
    fn supprime_les_champs_hors_liste_blanche() {
        let out = rewrite(json!({"model":"m","messages":[],"keep_alive":-1,"options":{"num_ctx":999999},"user":"x"}), &[]).unwrap();
        assert!(
            out.get("keep_alive").is_none()
                && out.get("options").is_none()
                && out.get("user").is_none()
        );
    }

    #[test]
    fn corps_invalides() {
        assert_eq!(
            rewrite_body(b"pas json", &[], 1).unwrap_err(),
            RewriteError::InvalidJson
        );
        assert_eq!(rewrite(json!([1]), &[]), Err(RewriteError::InvalidBody));
        assert_eq!(
            rewrite(json!({"messages":[]}), &[]),
            Err(RewriteError::InvalidBody)
        );
        assert_eq!(
            rewrite(json!({"model":"m"}), &[]),
            Err(RewriteError::InvalidBody)
        );
        assert_eq!(
            rewrite(json!({"model":"","messages":[]}), &[]),
            Err(RewriteError::InvalidBody)
        );
    }
}
