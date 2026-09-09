use std::env;

/// Configuration globale du process — pas de config *par organisation* : le modèle et l'URL de
/// l'API Synco viennent toujours du corps de chaque requête (envoyés par le frontend depuis les
/// paramètres IA de l'org), donc un seul déploiement de la passerelle peut servir n'importe
/// quelle organisation qui le pointe correctement.
///
/// L'URL d'Ollama, elle, est une décision de *déploiement* (où tourne Ollama par rapport à cette
/// passerelle), pas une décision par organisation — elle vit ici, pas dans les paramètres Synco.
/// Par défaut `http://127.0.0.1:11434` : c'est toujours vrai avec l'image tout-en-un (Ollama et
/// la passerelle dans le même conteneur). Pour un déploiement séparé (Dockerfile.gateway-only),
/// surchargez OLLAMA_URL avec l'adresse du conteneur/service Ollama.
#[derive(Clone, Debug)]
pub struct Config {
    pub port: u16,
    /// Origine(s) autorisées en CORS pour les requêtes du frontend Synco. "*" par défaut.
    pub allowed_origin: String,
    pub ollama_url: String,
}

impl Config {
    pub fn from_env() -> Self {
        let port = env::var("PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(8787);

        let allowed_origin = env::var("ALLOWED_ORIGIN").unwrap_or_else(|_| "*".to_string());
        let ollama_url = env::var("OLLAMA_URL").unwrap_or_else(|_| "http://127.0.0.1:11434".to_string());

        Self { port, allowed_origin, ollama_url }
    }
}
