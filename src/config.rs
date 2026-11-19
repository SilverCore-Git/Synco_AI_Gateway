use crate::ticket::AllowedOrgs;
use std::collections::HashSet;
use std::env;

/// Plafonds par défaut du relais (alignés sur `synco_api/src/routes/aiProxy.ts`, relevés pour un
/// historique d'agent avec outils).
pub const DEFAULT_MAX_BODY_BYTES: usize = 1024 * 1024;
pub const DEFAULT_MAX_TOKENS: u64 = 4096;

/// Configuration de déploiement (variables d'environnement). Aucune configuration par organisation.
#[derive(Clone, Debug)]
pub struct Config {
    pub port: u16,
    /// Origine CORS de l'app Synco ; `None` = aucun en-tête CORS (seul le runtime appelle).
    /// `Some("*")` n'existe que par choix explicite.
    pub allowed_origin: Option<String>,
    pub ollama_url: String,
    /// URL de l'API Synco, utilisée seulement pour lire `GET /api/ai/ticket-keys`.
    pub synco_api_url: String,
    /// URL canonique de CETTE passerelle : doit être égale à la revendication `gw` des tickets.
    pub gateway_url: String,
    pub allowed_orgs: AllowedOrgs,
    /// Modèles que la passerelle accepte de relayer ; vide = tout modèle présent sur Ollama.
    pub allowed_models: Vec<String>,
    pub max_body_bytes: usize,
    pub max_tokens: u64,
}

/// URL canonique d'une passerelle, identique à `canonicalGatewayUrl` de l'API : HTTPS, ni
/// identifiants, ni requête, ni fragment, origine en minuscules, chemin sans `/` final, ≤ 255.
pub fn canonical_gateway_url(raw: &str) -> Option<String> {
    let u = reqwest::Url::parse(raw).ok()?;
    if u.scheme() != "https"
        || !u.username().is_empty()
        || u.password().is_some()
        || u.query().is_some()
        || u.fragment().is_some()
    {
        return None;
    }
    let origin = u.origin().ascii_serialization();
    let path = u.path();
    let path = path.strip_suffix('/').unwrap_or(path);
    let canonical = format!("{origin}{path}");
    (canonical.encode_utf16().count() <= 255).then_some(canonical)
}

/// L'API est jointe en HTTPS vérifié ; HTTP seulement vers la boucle locale (développement).
fn valid_api_url(raw: &str) -> bool {
    let Ok(u) = reqwest::Url::parse(raw) else {
        return false;
    };
    if u.query().is_some() || u.fragment().is_some() || !u.username().is_empty() {
        return false;
    }
    match u.scheme() {
        "https" => true,
        "http" => matches!(u.host_str(), Some("localhost" | "127.0.0.1" | "[::1]")),
        _ => false,
    }
}

fn list(var: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    env::var(var)
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty() && seen.insert(s.to_string()))
        .map(str::to_string)
        .collect()
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        let port = env::var("PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(8787);
        let ollama_url =
            env::var("OLLAMA_URL").unwrap_or_else(|_| "http://127.0.0.1:11434".to_string());

        let synco_api_url = env::var("SYNCO_API_URL").map_err(|_| {
            "SYNCO_API_URL est requis (lecture des clés publiques de tickets)".to_string()
        })?;
        if !valid_api_url(&synco_api_url) {
            return Err(
                "SYNCO_API_URL doit être une URL https (http seulement vers localhost)".into(),
            );
        }

        let raw_gw = env::var("SYNCO_GATEWAY_URL").map_err(|_| "SYNCO_GATEWAY_URL est requis (URL publique de cette passerelle, telle que saisie dans Synco)".to_string())?;
        let gateway_url = canonical_gateway_url(&raw_gw).ok_or(
            "SYNCO_GATEWAY_URL doit être une URL https sans identifiants, requête ni fragment",
        )?;

        let orgs = list("SYNCO_ALLOWED_ORGS");
        let any_org = env::var("SYNCO_ALLOW_ANY_ORG").is_ok_and(|v| v == "true");
        let allowed_orgs = match (orgs.is_empty(), any_org) {
            (false, false) => AllowedOrgs::List(orgs),
            (false, true) => return Err("SYNCO_ALLOWED_ORGS et SYNCO_ALLOW_ANY_ORG=true sont exclusifs".into()),
            (true, true) => {
                tracing::warn!("SYNCO_ALLOW_ANY_ORG=true : TOUTE organisation Synco peut utiliser cette passerelle (non recommandé)");
                AllowedOrgs::Any
            }
            (true, false) => return Err("SYNCO_ALLOWED_ORGS est requis (identifiants d'organisations séparés par des virgules)".into()),
        };

        let allowed_origin = match env::var("ALLOWED_ORIGIN") {
            Ok(v) if v.trim().is_empty() => None,
            Ok(v) => Some(v.trim().to_string()),
            Err(_) => None,
        };

        let mut allowed_models = list("SYNCO_ALLOWED_MODELS");
        if allowed_models.is_empty() {
            allowed_models = list("OLLAMA_MODEL");
        }

        let max_body_bytes = env::var("SYNCO_MAX_BODY_BYTES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_MAX_BODY_BYTES);
        let max_tokens = env::var("SYNCO_MAX_TOKENS")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT_MAX_TOKENS);

        Ok(Self {
            port,
            allowed_origin,
            ollama_url,
            synco_api_url,
            gateway_url,
            allowed_orgs,
            allowed_models,
            max_body_bytes,
            max_tokens,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_canonique_comme_l_api() {
        assert_eq!(
            canonical_gateway_url("https://Gateway.Example.com:443/").as_deref(),
            Some("https://gateway.example.com")
        );
        assert_eq!(
            canonical_gateway_url("https://gateway.example.com/ia/").as_deref(),
            Some("https://gateway.example.com/ia")
        );
        assert_eq!(
            canonical_gateway_url("https://gateway.example.com:8443").as_deref(),
            Some("https://gateway.example.com:8443")
        );
    }

    #[test]
    fn url_canonique_refus() {
        for bad in [
            "http://gateway.example.com",
            "https://u:p@gateway.example.com",
            "https://gateway.example.com/?a=1",
            "https://gateway.example.com/#x",
            "pas une url",
        ] {
            assert!(canonical_gateway_url(bad).is_none(), "{bad}");
        }
        assert!(
            canonical_gateway_url(&format!("https://gateway.example.com/{}", "a".repeat(260)))
                .is_none()
        );
    }

    #[test]
    fn url_de_l_api_https_ou_boucle_locale() {
        assert!(valid_api_url("https://api.example.com"));
        assert!(valid_api_url("http://localhost:9000"));
        assert!(!valid_api_url("http://api.example.com"));
        assert!(!valid_api_url("ftp://api.example.com"));
    }
}
