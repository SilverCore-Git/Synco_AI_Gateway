//! Aucun journal ne contient de prompt, de réponse ni de ticket : abonné global capturant tout
//! (niveau TRACE, toutes cibles, y compris hyper et reqwest) pendant un échange complet.

mod common;
use common::*;
use std::io::Write;
use std::sync::{Arc, Mutex};
use tracing_subscriber::fmt::MakeWriter;

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("verrou").extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Capture {
    type Writer = Capture;
    fn make_writer(&'a self) -> Capture {
        self.clone()
    }
}

#[tokio::test]
async fn ni_prompt_ni_reponse_ni_ticket_dans_les_journaux() {
    let capture = Capture::default();
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_writer(capture.clone())
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .finish(),
    )
    .expect("abonné global");

    let key = TestKey::new("k1", 7);
    let gw = start_gateway(&key, Options::default()).await;
    let ticket = key.gateway_ticket(&Claims::valid());
    let payload_segment = ticket.split('.').nth(2).expect("contenu").to_string();
    let signature_segment = ticket.split('.').nth(3).expect("signature").to_string();

    let prompt = "PROMPT-SECRET-4f2a9c";
    let body = format!(
        r#"{{"model":"llama3.2","messages":[{{"role":"user","content":"{prompt}"}}],"stream":true}}"#
    );
    assert!(gw
        .chat(Some(&ticket), &body)
        .await
        .text()
        .await
        .expect("corps")
        .contains("salut"));
    // Cas d'erreur : ticket altéré, modèle refusé, corps invalide.
    let bad = format!("{ticket}x");
    gw.chat(Some(&bad), &body).await;
    gw.chat(Some(&ticket), &body.replace("llama3.2", "interdit-9d1e"))
        .await;
    gw.chat(Some(&ticket), &format!("{prompt} pas du json"))
        .await;
    // Ticket passé (à tort) dans la requête de l'URL : jamais journalisé non plus.
    let leaked = format!("/v1/chat/completions?ticket={ticket}");
    gw.http
        .post(gw.url(&leaked))
        .body(body.clone())
        .send()
        .await
        .expect("réponse");
    gw.http
        .get(gw.url("/models"))
        .bearer_auth(&ticket)
        .send()
        .await
        .expect("réponse");

    let logs = String::from_utf8_lossy(&capture.0.lock().expect("verrou")).into_owned();
    assert!(
        logs.len() > 200,
        "la capture est vide : le test ne prouverait rien"
    );
    assert!(
        logs.contains("/v1/chat/completions"),
        "les requêtes devraient être tracées (méthode et chemin)"
    );
    for secret in [
        prompt,
        "salut",
        ticket.as_str(),
        payload_segment.as_str(),
        signature_segment.as_str(),
        "Bearer",
    ] {
        assert!(
            !logs.contains(secret),
            "journal contenant une donnée sensible : {secret}"
        );
    }
}
