//! Synco AI Gateway : proxy LLM authentifié devant Ollama (docs/plan P7).
//!
//! Trois routes seulement : `GET /health`, `GET /models`, `POST /v1/chat/completions`. Les deux
//! dernières exigent un ticket `sgt1` (`Authorization: Bearer`), vérifié localement. Toute autre
//! route répond 404 sans jamais atteindre Ollama. Rien de ce qui transite (prompt, réponse,
//! ticket) n'est journalisé.

pub mod app;
pub mod config;
pub mod keys;
pub mod relay;
pub mod ticket;
