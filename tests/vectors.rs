//! Rejoue TOUS les vecteurs partagés `tickets-gateway.json` (générés par le code réel de
//! synco_api, `tests/vectors/ai-runtime/generate.ts`). Un seul cas divergent fait échouer le test.

use serde_json::Value;
use synco_ai_gateway::keys::parse_keys_document;
use synco_ai_gateway::ticket::{
    decode_b64u_exact, KeyMap, Verifier, CLOCK_SKEW_SECS, MAX_LENGTH, MAX_LIFETIME_SECS,
};

const VECTORS: &str = include_str!("vectors/tickets-gateway.json");

#[test]
fn tous_les_vecteurs_sgt1_sont_rejoues() {
    let doc: Value = serde_json::from_str(VECTORS).expect("JSON valide");

    // La politique du fichier est celle codée dans la passerelle.
    let policy = &doc["policy"];
    assert_eq!(policy["clock_skew_secs"], CLOCK_SKEW_SECS);
    assert_eq!(policy["max_lifetime_secs"], MAX_LIFETIME_SECS);
    assert_eq!(policy["max_length"], MAX_LENGTH as u64);

    let mut keys = KeyMap::new();
    for (kid, v) in doc["publicKeys"].as_object().expect("publicKeys") {
        let bytes = decode_b64u_exact(v.as_str().expect("b64u"), 32).expect("clé de 32 octets");
        keys.insert(kid.clone(), bytes.try_into().expect("32 octets"));
    }

    let cases = doc["cases"].as_array().expect("cases");
    assert!(
        cases.len() >= 39,
        "le fichier de vecteurs a perdu des cas : {}",
        cases.len()
    );

    let mut seen = std::collections::HashSet::new();
    let mut failures = Vec::new();
    let mut tested_ok = 0;
    for case in cases {
        let name = case["name"].as_str().expect("name");
        assert!(
            seen.insert(name.to_string()),
            "nom de cas dupliqué : {name}"
        );
        let verifier = Verifier {
            gateway_url: case["gatewayUrl"].as_str().expect("gatewayUrl").to_string(),
            allowed_orgs: case["allowedOrgs"]
                .as_array()
                .expect("allowedOrgs")
                .iter()
                .map(|o| o.as_str().expect("org").to_string())
                .collect(),
        };
        let now = case["now"].as_u64().expect("now");
        let expect = case["expect"].as_str().expect("expect");
        let got = match verifier.verify(case["ticket"].as_str().expect("ticket"), &keys, now) {
            Ok(_) => "ok",
            Err(e) => e.code(),
        };
        if got == "ok" {
            tested_ok += 1;
        }
        if got != expect {
            failures.push(format!("{name}: attendu {expect}, obtenu {got}"));
        }
    }
    assert!(
        failures.is_empty(),
        "cas divergents :\n{}",
        failures.join("\n")
    );
    assert!(tested_ok >= 4, "les cas positifs ont disparu");
}

#[test]
fn le_cache_de_cles_lit_les_cles_du_fichier_de_vecteurs_comme_l_api_les_publie() {
    let doc: Value = serde_json::from_str(VECTORS).expect("JSON valide");
    let published = serde_json::json!({ "v": 1, "keys": doc["publicKeys"] });
    let keys = parse_keys_document(published.to_string().as_bytes()).expect("document accepté");
    assert!(keys.contains_key("vec-1"));
}

#[test]
fn documents_de_cles_invalides_refuses() {
    let good = "lHXGzw7aNAV1KNhpR4HsySrWOdfivSd2HtPvdJJL6wc";
    for bad in [
        r#"{"v":2,"keys":{"a":"K"}}"#.replace('K', good),
        r#"{"v":1,"keys":{}}"#.to_string(),
        r#"{"v":1,"keys":{"A":"K"}}"#.replace('K', good),
        r#"{"v":1,"keys":{"a":"court"}}"#.to_string(),
        r#"{"v":1,"keys":{"a":"K"},"extra":1}"#.replace('K', good),
        r#"{"keys":{"a":"K"}}"#.replace('K', good),
        "pas json".to_string(),
    ] {
        assert!(
            parse_keys_document(bad.as_bytes()).is_none(),
            "accepté à tort : {bad}"
        );
    }
}
