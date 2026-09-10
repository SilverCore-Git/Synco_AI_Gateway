use aes_gcm::aead::{Aead, Generate, KeyInit, Nonce, Payload};
use aes_gcm::{Aes256Gcm, Key};
use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use serde_json::Value;

/// Format défini dans E2EE_PLAN.md §2 — ne pas diverger sans répercuter le changement dans les
/// plans jumeaux `synco_api`/`synco_app`.
const VERSION_PREFIX: &str = "gcm1:";
const NONCE_LEN: usize = 12;
const TAG_LEN: usize = 16;

/// Clé AES-256 brute (32 octets), transmise par le navigateur à chaque requête via le header
/// `X-Session-Key`. Jamais persistée, jamais logguée — `Debug` masque volontairement la valeur.
pub struct SessionKey([u8; 32]);

impl std::fmt::Debug for SessionKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SessionKey(***)")
    }
}

#[derive(Debug)]
pub enum CryptoError {
    /// Header absent, base64 invalide, ou décodé à une longueur ≠ 32 octets.
    MissingOrInvalidHeader,
    /// Préfixe de version inconnu (pas "gcm1:").
    UnknownVersion,
    InvalidBase64,
    /// Moins de NONCE_LEN + TAG_LEN octets après décodage base64.
    InvalidLength,
    /// Tag d'authentification GCM invalide — mauvaise clé ou donnée falsifiée.
    AuthenticationFailed,
    /// Le texte déchiffré n'est pas le JSON attendu pour un champ Value.
    InvalidJson,
}

impl std::fmt::Display for CryptoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let msg = match self {
            CryptoError::MissingOrInvalidHeader => "Header X-Session-Key manquant ou invalide.",
            CryptoError::UnknownVersion => "Valeur chiffrée : préfixe de version inconnu.",
            CryptoError::InvalidBase64 => "Valeur chiffrée : base64 invalide.",
            CryptoError::InvalidLength => "Valeur chiffrée : longueur insuffisante.",
            CryptoError::AuthenticationFailed => "Échec d'authentification GCM (clé invalide ou donnée falsifiée).",
            CryptoError::InvalidJson => "Valeur déchiffrée : JSON invalide.",
        };
        write!(f, "{msg}")
    }
}

impl std::error::Error for CryptoError {}

/// Décode le header `X-Session-Key` (base64 standard d'une clé AES-256 brute, 32 octets).
/// Fail-closed : toute erreur de format doit se traduire par un 400 côté appelant, jamais par un
/// retour silencieux vers un mode "session en clair" (E2EE_PLAN.md §3).
pub fn parse_session_key_header(value: &str) -> Result<SessionKey, CryptoError> {
    let bytes = STANDARD.decode(value.trim()).map_err(|_| CryptoError::MissingOrInvalidHeader)?;
    let arr: [u8; 32] = bytes.try_into().map_err(|_| CryptoError::MissingOrInvalidHeader)?;
    Ok(SessionKey(arr))
}

fn build_aad(session_id: &str, message_id: &str, field_name: &str) -> String {
    format!("{session_id}:{message_id}:{field_name}")
}

fn cipher_for(key: &SessionKey) -> Aes256Gcm {
    Aes256Gcm::new(&Key::<Aes256Gcm>::from(key.0))
}

/// Chiffre `plaintext` et retourne la valeur "sur le fil" `"gcm1:" + base64(nonce || ciphertext || tag)`.
pub fn encrypt_field(key: &SessionKey, session_id: &str, message_id: &str, field_name: &str, plaintext: &str) -> String {
    let cipher = cipher_for(key);
    let nonce = Nonce::<Aes256Gcm>::generate();
    let aad = build_aad(session_id, message_id, field_name);

    let ciphertext = cipher
        .encrypt(&nonce, Payload { msg: plaintext.as_bytes(), aad: aad.as_bytes() })
        .expect("le chiffrement AES-GCM ne peut pas échouer avec une clé/nonce valides");

    let mut wire = Vec::with_capacity(NONCE_LEN + ciphertext.len());
    wire.extend_from_slice(&nonce);
    wire.extend_from_slice(&ciphertext);

    format!("{VERSION_PREFIX}{}", STANDARD.encode(wire))
}

/// Inverse de [`encrypt_field`]. Retourne une erreur typée distincte par cause (E2EE_PLAN.md §4).
pub fn decrypt_field(key: &SessionKey, session_id: &str, message_id: &str, field_name: &str, wire_value: &str) -> Result<String, CryptoError> {
    let b64 = wire_value.strip_prefix(VERSION_PREFIX).ok_or(CryptoError::UnknownVersion)?;
    let raw = STANDARD.decode(b64).map_err(|_| CryptoError::InvalidBase64)?;

    if raw.len() < NONCE_LEN + TAG_LEN {
        return Err(CryptoError::InvalidLength);
    }

    let (nonce_bytes, ciphertext) = raw.split_at(NONCE_LEN);
    let nonce = Nonce::<Aes256Gcm>::try_from(nonce_bytes).map_err(|_| CryptoError::InvalidLength)?;
    let aad = build_aad(session_id, message_id, field_name);

    let cipher = cipher_for(key);
    let plaintext_bytes = cipher
        .decrypt(&nonce, Payload { msg: ciphertext, aad: aad.as_bytes() })
        .map_err(|_| CryptoError::AuthenticationFailed)?;

    String::from_utf8(plaintext_bytes).map_err(|_| CryptoError::AuthenticationFailed)
}

/// Comme [`decrypt_field`], mais traite une valeur sans préfixe "gcm1:" comme déjà en clair plutôt
/// que comme une erreur — c'est le mécanisme de migration : une session écrite avant l'activation
/// du chiffrement (ou par un provider différent) a des champs en clair, distinguables au niveau du
/// champ lui-même sans avoir besoin d'un marqueur au niveau de la session (cf. E2EE_PLAN.md §6 —
/// l'idée d'un marqueur `encryptionScheme` coordonné avec `synco_api` reste possible plus tard,
/// mais cette détection par-champ ne nécessite aucune coordination inter-repo pour fonctionner).
pub fn decrypt_field_if_encrypted(key: &SessionKey, session_id: &str, message_id: &str, field_name: &str, wire_value: &str) -> Result<String, CryptoError> {
    if wire_value.starts_with(VERSION_PREFIX) {
        decrypt_field(key, session_id, message_id, field_name, wire_value)
    } else {
        Ok(wire_value.to_string())
    }
}

/// Sérialise `value` en JSON puis chiffre — pour `tool_result`/`arguments`/`pending_tool_call.args`.
pub fn encrypt_json_field(key: &SessionKey, session_id: &str, message_id: &str, field_name: &str, value: &Value) -> String {
    let plaintext = serde_json::to_string(value).unwrap_or_else(|_| "null".to_string());
    encrypt_field(key, session_id, message_id, field_name, &plaintext)
}

/// Inverse de [`encrypt_json_field`], avec la même tolérance "déjà en clair" que
/// [`decrypt_field_if_encrypted`] : si `value` n'est pas une `Value::String` préfixée "gcm1:", elle
/// est renvoyée telle quelle (session/JSON pas encore chiffrée).
pub fn decrypt_json_field_if_encrypted(key: &SessionKey, session_id: &str, message_id: &str, field_name: &str, value: Value) -> Result<Value, CryptoError> {
    let Value::String(s) = &value else {
        return Ok(value);
    };
    if !s.starts_with(VERSION_PREFIX) {
        return Ok(value);
    }
    let plaintext = decrypt_field(key, session_id, message_id, field_name, s)?;
    serde_json::from_str(&plaintext).map_err(|_| CryptoError::InvalidJson)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_key() -> SessionKey {
        // E2EE_PLAN.md §2 truncates this hex string by one trailing digit (its own caveat warns
        // the abbreviated display might not be exact) — recovered here from the base64 form given
        // alongside it (`2C8mRtERMnD1j6Pa9k3n6fZCUxI6Dsf9uqJONAgdeRE=`), which decodes cleanly to
        // 32 bytes ending in `...d7911` rather than the plan's `...d791`.
        let hex = "d82f2646d1113270f58fa3daf64de7e9f64253123a0ec7fdbaa24e34081d7911";
        let bytes: Vec<u8> = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect();
        assert_eq!(bytes.len(), 32);
        SessionKey(bytes.try_into().unwrap())
    }

    #[test]
    fn round_trip_encrypt_decrypt() {
        let key = test_key();
        let wire = encrypt_field(&key, "sess_1", "msg_1", "content", "Bonjour le monde");
        assert!(wire.starts_with(VERSION_PREFIX));
        let plaintext = decrypt_field(&key, "sess_1", "msg_1", "content", &wire).unwrap();
        assert_eq!(plaintext, "Bonjour le monde");
    }

    /// Vecteur de test d'E2EE_PLAN.md §2 — vérifie l'interopérabilité avec les implémentations
    /// jumelles (synco_api en TS) sans jamais faire tourner les deux ensemble.
    #[test]
    fn plan_test_vector_decrypts_correctly() {
        let key = test_key();
        let session_id = "sess_test0000000000000000000001";
        let message_id = "msg_test00000000000000000000001";
        let field_name = "content";
        let wire_value = "gcm1:ECTIwmhA1Bb1C1ByL8IECZj8SkBl2TYmk9665e/jnrCQNjYdR+CMirRU75qIq5pxnrallgkaWQtrQreT8eCSCf6JAzUrY5mTb/Ez42WxSw==";

        let plaintext = decrypt_field(&key, session_id, message_id, field_name, wire_value).unwrap();
        assert_eq!(plaintext, "Bonjour, ceci est un message de test pour Synco AI.");

        let b64 = wire_value.strip_prefix(VERSION_PREFIX).unwrap();
        let raw = STANDARD.decode(b64).unwrap();
        let nonce_hex: String = raw[..NONCE_LEN].iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(nonce_hex, "1024c8c26840d416f50b5072");
    }

    #[test]
    fn corrupted_tag_fails_authentication() {
        let key = test_key();
        let wire = encrypt_field(&key, "sess_1", "msg_1", "content", "secret");
        let mut bytes = wire.into_bytes();
        let last = bytes.len() - 1;
        bytes[last] = if bytes[last] == b'A' { b'B' } else { b'A' };
        let corrupted = String::from_utf8(bytes).unwrap();

        let err = decrypt_field(&key, "sess_1", "msg_1", "content", &corrupted).unwrap_err();
        assert!(matches!(err, CryptoError::AuthenticationFailed | CryptoError::InvalidBase64));
    }

    #[test]
    fn wrong_aad_fails_authentication() {
        let key = test_key();
        let wire = encrypt_field(&key, "sess_1", "msg_1", "content", "secret");
        let err = decrypt_field(&key, "sess_1", "msg_OTHER", "content", &wire).unwrap_err();
        assert!(matches!(err, CryptoError::AuthenticationFailed));
    }

    #[test]
    fn plaintext_field_passes_through_when_not_yet_encrypted() {
        let key = test_key();
        let plaintext = decrypt_field_if_encrypted(&key, "sess_1", "msg_1", "content", "texte en clair").unwrap();
        assert_eq!(plaintext, "texte en clair");
    }

    #[test]
    fn json_round_trip() {
        let key = test_key();
        let value = serde_json::json!({ "title": "Créer une tâche", "count": 3 });
        let wire = encrypt_json_field(&key, "sess_1", "msg_1", "tool_result", &value);
        let decrypted = decrypt_json_field_if_encrypted(&key, "sess_1", "msg_1", "tool_result", Value::String(wire)).unwrap();
        assert_eq!(decrypted, value);
    }

    #[test]
    fn json_plain_object_passes_through_when_not_yet_encrypted() {
        let key = test_key();
        let value = serde_json::json!({ "already": "plaintext" });
        let decrypted = decrypt_json_field_if_encrypted(&key, "sess_1", "msg_1", "tool_result", value.clone()).unwrap();
        assert_eq!(decrypted, value);
    }
}
