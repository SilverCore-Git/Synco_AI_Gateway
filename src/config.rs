use std::collections::HashSet;
use std::env;

/// Plafonds par défaut du relais (alignés sur `synco_api/src/routes/aiProxy.ts`, relevés pour un
/// historique d'agent avec outils).
pub const DEFAULT_MAX_BODY_BYTES: usize = 1024 * 1024;
pub const DEFAULT_MAX_TOKENS: u64 = 4096;
/// Requêtes simultanées vers Ollama : au total, et par utilisateur (`sub` du ticket).
pub const DEFAULT_MAX_CONCURRENT: usize = 16;
pub const DEFAULT_MAX_CONCURRENT_PER_USER: usize = 4;

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
    /// Non vide (02 §5 étape 8).
    pub allowed_orgs: Vec<String>,
    /// Modèles que la passerelle accepte de relayer ; vide = tout modèle présent sur Ollama.
    pub allowed_models: Vec<String>,
    pub max_body_bytes: usize,
    pub max_tokens: u64,
    pub max_concurrent: usize,
    pub max_concurrent_per_user: usize,
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

/// Origine CORS : `*` ou une origine unique `scheme://host[:port]` (minuscules, sans chemin,
/// sans `/` final, sans virgule).
fn valid_origin(raw: &str) -> bool {
    if raw == "*" {
        return true;
    }
    reqwest::Url::parse(raw).is_ok_and(|u| {
        matches!(u.scheme(), "http" | "https") && u.origin().ascii_serialization() == raw
    })
}

fn list(raw: Option<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    raw.unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty() && seen.insert(s.to_string()))
        .map(str::to_string)
        .collect()
}

fn number<T: std::str::FromStr + PartialOrd + Default>(raw: Option<String>, default: T) -> T {
    raw.and_then(|v| v.parse().ok())
        .filter(|n| *n > T::default())
        .unwrap_or(default)
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        Self::from_lookup(|k| env::var(k).ok())
    }

    /// Lecture de la configuration par une fonction de recherche (testable sans toucher l'environnement).
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let port = get("PORT").and_then(|v| v.parse().ok()).unwrap_or(8787);
        let ollama_url = get("OLLAMA_URL").unwrap_or_else(|| "http://127.0.0.1:11434".to_string());

        let synco_api_url = get("SYNCO_API_URL")
            .ok_or("SYNCO_API_URL est requis (lecture des clés publiques de tickets)")?;
        if !valid_api_url(&synco_api_url) {
            return Err(
                "SYNCO_API_URL doit être une URL https (http seulement vers localhost)".into(),
            );
        }

        let raw_gw = get("SYNCO_GATEWAY_URL").ok_or("SYNCO_GATEWAY_URL est requis (URL publique de cette passerelle, telle que saisie dans Synco)")?;
        let gateway_url = canonical_gateway_url(&raw_gw).ok_or(
            "SYNCO_GATEWAY_URL doit être une URL https sans identifiants, requête ni fragment",
        )?;

        let allowed_orgs = list(get("SYNCO_ALLOWED_ORGS"));
        if allowed_orgs.is_empty() {
            return Err("SYNCO_ALLOWED_ORGS est requis et non vide (identifiants d'organisations séparés par des virgules)".into());
        }

        let allowed_origin = match get("ALLOWED_ORIGIN").map(|v| v.trim().to_string()) {
            None => None,
            Some(v) if v.is_empty() => None,
            Some(v) if valid_origin(&v) => Some(v),
            Some(_) => return Err("ALLOWED_ORIGIN doit être une origine unique de la forme scheme://host[:port] (minuscules, sans chemin, sans / final, sans virgule), ou *".into()),
        };

        let mut allowed_models = list(get("SYNCO_ALLOWED_MODELS"));
        if allowed_models.is_empty() {
            allowed_models = list(get("OLLAMA_MODEL"));
        }

        Ok(Self {
            port,
            allowed_origin,
            ollama_url,
            synco_api_url,
            gateway_url,
            allowed_orgs,
            allowed_models,
            max_body_bytes: number(get("SYNCO_MAX_BODY_BYTES"), DEFAULT_MAX_BODY_BYTES),
            max_tokens: number(get("SYNCO_MAX_TOKENS"), DEFAULT_MAX_TOKENS),
            max_concurrent: number(get("SYNCO_MAX_CONCURRENT_REQUESTS"), DEFAULT_MAX_CONCURRENT),
            max_concurrent_per_user: number(
                get("SYNCO_MAX_CONCURRENT_PER_USER"),
                DEFAULT_MAX_CONCURRENT_PER_USER,
            ),
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

    fn load(extra: &[(&str, &str)]) -> Result<Config, String> {
        let mut vars: std::collections::HashMap<String, String> = [
            ("SYNCO_API_URL", "https://api.example.com"),
            ("SYNCO_GATEWAY_URL", "https://gateway.example.com"),
            ("SYNCO_ALLOWED_ORGS", "org1, org2"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        for (k, v) in extra {
            vars.insert(k.to_string(), v.to_string());
        }
        Config::from_lookup(|k| vars.get(k).cloned())
    }

    #[test]
    fn config_minimale_valide() {
        let c = load(&[]).unwrap();
        assert_eq!(c.allowed_orgs, ["org1", "org2"]);
        assert_eq!(
            (c.max_concurrent, c.max_concurrent_per_user),
            (DEFAULT_MAX_CONCURRENT, DEFAULT_MAX_CONCURRENT_PER_USER)
        );
        assert!(c.allowed_origin.is_none());
    }

    #[test]
    fn liste_d_orgs_obligatoire_et_non_vide() {
        assert!(load(&[("SYNCO_ALLOWED_ORGS", "")]).is_err());
        assert!(load(&[("SYNCO_ALLOWED_ORGS", " , ,")]).is_err());
        let mut vars = std::collections::HashMap::new();
        vars.insert("SYNCO_API_URL", "https://api.example.com");
        vars.insert("SYNCO_GATEWAY_URL", "https://gateway.example.com");
        assert!(Config::from_lookup(|k| vars.get(k).map(|v| v.to_string())).is_err());
        // L'ancienne échappatoire n'existe plus : elle ne change rien.
        assert!(load(&[("SYNCO_ALLOWED_ORGS", ""), ("SYNCO_ALLOW_ANY_ORG", "true")]).is_err());
    }

    #[test]
    fn allowed_origin_validee() {
        for ok in ["https://app.example.com", "http://localhost:5174", "*"] {
            assert_eq!(
                load(&[("ALLOWED_ORIGIN", ok)])
                    .unwrap()
                    .allowed_origin
                    .as_deref(),
                Some(ok)
            );
        }
        for bad in [
            "https://app.example.com/",
            "https://app.example.com/chemin",
            "https://a.example.com,https://b.example.com",
            "app.example.com",
            "ftp://app.example.com",
            "https://App.Example.com",
            "https://u@app.example.com",
        ] {
            assert!(load(&[("ALLOWED_ORIGIN", bad)]).is_err(), "{bad}");
        }
    }

    #[test]
    fn limites_de_concurrence_configurables_et_bornees_a_positif() {
        let c = load(&[
            ("SYNCO_MAX_CONCURRENT_REQUESTS", "3"),
            ("SYNCO_MAX_CONCURRENT_PER_USER", "0"),
        ])
        .unwrap();
        assert_eq!(
            (c.max_concurrent, c.max_concurrent_per_user),
            (3, DEFAULT_MAX_CONCURRENT_PER_USER)
        );
    }
}
