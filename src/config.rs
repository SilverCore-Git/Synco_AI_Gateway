use std::env;

/// Configuration globale du process — pas de config *par organisation* : le modèle vient
/// toujours du corps de chaque requête (envoyé par le frontend depuis les paramètres IA de
/// l'org), donc un seul déploiement de la passerelle peut servir n'importe quelle organisation
/// qui le pointe correctement.
///
/// L'URL d'Ollama et l'URL de l'API Synco, elles, sont des décisions de *déploiement* (où tourne
/// Ollama et où joindre Synco depuis LA PASSERELLE — pas depuis le navigateur, ce n'est pas la
/// même chose dès que la passerelle tourne dans un conteneur) — elles vivent ici, pas dans les
/// paramètres Synco.
///
/// - `OLLAMA_URL` par défaut `http://127.0.0.1:11434` : toujours vrai avec l'image tout-en-un.
/// - `SYNCO_API_URL` optionnel : si absent, on retombe sur la valeur envoyée par le frontend
///   (`VITE_API_URL` de son point de vue) — ce qui casse dès que la passerelle tourne dans un
///   conteneur et que "localhost" n'y désigne plus la même machine que dans le navigateur.
///   À renseigner explicitement dans ce cas (ex: `http://host.docker.internal:9000` si l'API
///   Synco tourne sur la machine hôte, en dehors du conteneur de la passerelle).
#[derive(Clone, Debug)]
pub struct Config {
    pub port: u16,
    /// Origine(s) autorisées en CORS pour les requêtes du frontend Synco. "*" par défaut.
    pub allowed_origin: String,
    pub ollama_url: String,
    pub synco_api_url: Option<String>,
}

impl Config {
    pub fn from_env() -> Self {
        let port = env::var("PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(8787);

        let allowed_origin = env::var("ALLOWED_ORIGIN").unwrap_or_else(|_| "*".to_string());
        let ollama_url = env::var("OLLAMA_URL").unwrap_or_else(|_| "http://127.0.0.1:11434".to_string());
        let synco_api_url = env::var("SYNCO_API_URL").ok();

        Self { port, allowed_origin, ollama_url, synco_api_url }
    }
}
