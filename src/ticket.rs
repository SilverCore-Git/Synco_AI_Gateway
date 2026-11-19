//! Vérification locale des tickets `sgt1` (synco_ai_runtime/docs/plan/02-tickets.md §5).
//!
//! Même ordre de contrôles que le vérificateur de référence TS
//! (`synco_api/src/services/aiRuntime/runtimeTickets.ts::verifyAiTicket`), rejoué sur les vecteurs
//! partagés `tests/vectors/tickets-gateway.json`. Fonction pure : les clés et l'horloge sont
//! passées en paramètre.

use aws_lc_rs::signature::{UnparsedPublicKey, ED25519};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde_json::{Map, Value};
use std::collections::HashMap;

/// Domaine de signature de `sgt1` (octet nul final inclus).
pub const GATEWAY_TICKET_DOMAIN: &[u8] = b"synco-ai-gateway-ticket\0";
pub const PREFIX: &str = "sgt1";
pub const MAX_LENGTH: usize = 2048;
pub const CLOCK_SKEW_SECS: u64 = 30;
pub const MAX_LIFETIME_SECS: u64 = 900;
const MAX_SAFE_INT: f64 = 9_007_199_254_740_991.0;

/// Clés publiques Ed25519 (32 octets) par `kid`.
pub type KeyMap = HashMap<String, [u8; 32]>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TicketError {
    Missing,
    Invalid,
    UnknownKey,
    Expired,
    WrongOperation,
    WrongAudience,
    WrongOrg,
}

impl TicketError {
    /// Code machine stable (02 §10), seul renvoyé au client.
    pub fn code(self) -> &'static str {
        match self {
            Self::Missing => "ticket_missing",
            Self::Invalid => "ticket_invalid",
            Self::UnknownKey => "ticket_unknown_key",
            Self::Expired => "ticket_expired",
            Self::WrongOperation => "ticket_wrong_operation",
            Self::WrongAudience => "ticket_wrong_audience",
            Self::WrongOrg => "ticket_wrong_org",
        }
    }
}

/// Ce que connaît la passerelle : sa propre URL canonique et les orgs admises.
#[derive(Clone, Debug)]
pub struct Verifier {
    pub gateway_url: String,
    /// `SYNCO_ALLOWED_ORGS` (02 §5 étape 8). Une liste vide n'autorise personne (échec fermé).
    pub allowed_orgs: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Claims {
    pub sub: String,
    pub org: String,
    pub rt: Option<String>,
    pub iat: u64,
    pub exp: u64,
}

pub fn is_kid(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 32
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn is_b64u_charset(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// `^[A-Za-z0-9_-]{1,64}$` (00 §3.2).
fn is_identifier(v: &Value) -> bool {
    matches!(v, Value::String(s) if s.len() <= 64 && is_b64u_charset(s))
}

/// UUID en minuscules (01 §1).
fn is_uuid(v: &Value) -> bool {
    let Value::String(s) = v else { return false };
    let b = s.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => *c == b'-',
            _ => c.is_ascii_digit() || (b'a'..=b'f').contains(c),
        })
}

/// Entier non signé ≤ 2^53 − 1 ; comme `Number.isSafeInteger` côté TS, `1000.0` est accepté.
fn safe_uint(v: &Value) -> Option<u64> {
    let Value::Number(n) = v else { return None };
    if let Some(u) = n.as_u64() {
        return (u as f64 <= MAX_SAFE_INT).then_some(u);
    }
    let f = n.as_f64()?;
    (f >= 0.0 && f.fract() == 0.0 && f <= MAX_SAFE_INT).then_some(f as u64)
}

fn parse_claims(json: &[u8]) -> Option<(Claims, String)> {
    let Value::Object(obj) = serde_json::from_slice::<Value>(json).ok()? else {
        return None;
    };
    const REQUIRED: [&str; 6] = ["op", "sub", "org", "gw", "iat", "exp"];
    if obj
        .keys()
        .any(|k| !REQUIRED.contains(&k.as_str()) && k != "rt")
    {
        return None;
    }
    let get = |obj: &Map<String, Value>, k: &str| obj.get(k).cloned();
    for k in REQUIRED {
        obj.get(k)?;
    }
    let op = match obj.get("op")? {
        Value::String(s) if s == "connect" || s == "llm" => s.clone(),
        _ => return None,
    };
    let (sub, org) = (get(&obj, "sub")?, get(&obj, "org")?);
    if !is_identifier(&sub) || !is_identifier(&org) {
        return None;
    }
    let iat = safe_uint(obj.get("iat")?)?;
    let exp = safe_uint(obj.get("exp")?)?;
    let Value::String(gw) = obj.get("gw")? else {
        return None;
    };
    if gw.is_empty() || gw.encode_utf16().count() > 255 {
        return None;
    }
    let rt = match obj.get("rt") {
        None => None,
        Some(v) if is_uuid(v) => v.as_str().map(str::to_owned),
        Some(_) => return None,
    };
    let claims = Claims {
        sub: sub.as_str()?.to_owned(),
        org: org.as_str()?.to_owned(),
        rt,
        iat,
        exp,
    };
    Some((claims, format!("{op}\0{gw}")))
}

/// Extrait le `kid` d'un ticket sans rien valider d'autre (pour décider d'un rafraîchissement
/// des clés). `None` si la forme est déjà invalide : le vérificateur répondra de toute façon.
pub fn peek_kid(ticket: &str) -> Option<&str> {
    let mut parts = ticket.split('.');
    let (prefix, kid) = (parts.next()?, parts.next()?);
    (prefix == PREFIX && is_kid(kid)).then_some(kid)
}

impl Verifier {
    /// Vérifie `ticket` dans l'ordre de 02 §5. Jamais de panique sur une entrée hostile.
    pub fn verify(&self, ticket: &str, keys: &KeyMap, now: u64) -> Result<Claims, TicketError> {
        use TicketError::*;

        // 1. Longueur, quatre parties, préfixe.
        if ticket.is_empty() {
            return Err(Missing);
        }
        if ticket.len() > MAX_LENGTH {
            return Err(Invalid);
        }
        let parts: Vec<&str> = ticket.split('.').collect();
        let [prefix, kid, payload, signature_b64] = parts[..] else {
            return Err(Invalid);
        };
        if prefix != PREFIX || !is_kid(kid) || !is_b64u_charset(payload) {
            return Err(Invalid);
        }

        // 2. kid connu.
        let key = keys.get(kid).ok_or(UnknownKey)?;

        // 3. Signature avec le domaine de la passerelle, AVANT tout décodage JSON.
        let signature = decode_b64u_exact(signature_b64, 64).ok_or(Invalid)?;
        let mut signed = Vec::with_capacity(
            GATEWAY_TICKET_DOMAIN.len() + prefix.len() + kid.len() + payload.len() + 2,
        );
        signed.extend_from_slice(GATEWAY_TICKET_DOMAIN);
        signed.extend_from_slice(format!("{prefix}.{kid}.{payload}").as_bytes());
        UnparsedPublicKey::new(&ED25519, key)
            .verify(&signed, &signature)
            .map_err(|_| Invalid)?;

        // 4. JSON strict (le b64u doit être canonique : ré-encodage identique).
        let payload_bytes = URL_SAFE_NO_PAD.decode(payload).map_err(|_| Invalid)?;
        if URL_SAFE_NO_PAD.encode(&payload_bytes) != payload {
            return Err(Invalid);
        }
        let (claims, op_and_gw) = parse_claims(&payload_bytes).ok_or(Invalid)?;
        let (op, gw) = op_and_gw.split_once('\0').ok_or(Invalid)?;

        // 5. Fenêtre de validité, durée maximale.
        if now + CLOCK_SKEW_SECS < claims.iat {
            return Err(Invalid);
        }
        if now >= claims.exp.saturating_add(CLOCK_SKEW_SECS) {
            return Err(Expired);
        }
        if claims.exp.saturating_sub(claims.iat) > MAX_LIFETIME_SECS {
            return Err(Invalid);
        }

        // 6. Opération.
        if op != "llm" {
            return Err(WrongOperation);
        }

        // 7-8. Destinataire : URL canonique de CETTE passerelle ; org autorisée.
        if gw != self.gateway_url {
            return Err(WrongAudience);
        }
        if !self.allowed_orgs.contains(&claims.org) {
            return Err(WrongOrg);
        }
        Ok(claims)
    }
}

/// b64u canonique (sans remplissage, ré-encodage identique) de `len` octets exactement.
pub fn decode_b64u_exact(value: &str, len: usize) -> Option<Vec<u8>> {
    if !is_b64u_charset(value) {
        return None;
    }
    let bytes = URL_SAFE_NO_PAD.decode(value).ok()?;
    (bytes.len() == len && URL_SAFE_NO_PAD.encode(&bytes) == value).then_some(bytes)
}
