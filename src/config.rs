use std::env;

/// Configuration globale du process — pas de config par organisation ici : l'URL Ollama, le
/// modèle et l'URL de l'API Synco à contacter viennent du corps de chaque requête (envoyés par
/// le frontend depuis les paramètres IA de l'org), donc un seul déploiement de la passerelle peut
/// servir n'importe quelle organisation qui le pointe correctement.
#[derive(Clone, Debug)]
pub struct Config {
    pub port: u16,
    /// Origine(s) autorisées en CORS pour les requêtes du frontend Synco. "*" par défaut.
    pub allowed_origin: String,
}

impl Config {
    pub fn from_env() -> Self {
        let port = env::var("PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(8787);

        let allowed_origin = env::var("ALLOWED_ORIGIN").unwrap_or_else(|_| "*".to_string());

        Self { port, allowed_origin }
    }
}
