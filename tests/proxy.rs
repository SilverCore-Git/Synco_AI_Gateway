mod common;
use common::*;
use std::time::Duration;
use synco_ai_gateway::keys::KeyStoreConfig;

async fn code(resp: reqwest::Response) -> (u16, String) {
    let status = resp.status().as_u16();
    let v: serde_json::Value = resp.json().await.expect("JSON");
    (status, v["error"].as_str().unwrap_or("").to_string())
}

#[tokio::test]
async fn sans_ticket_ou_ticket_invalide_401_et_ollama_non_appele() {
    let key = TestKey::new("k1", 7);
    let gw = start_gateway(&key, Options::default()).await;
    let n = now();

    let mut cases: Vec<(&str, Option<String>, &str)> = vec![
        ("sans ticket", None, "ticket_missing"),
        ("charabia", Some("n'importe quoi".into()), "ticket_invalid"),
        (
            "expiré",
            Some(key.gateway_ticket(&Claims {
                iat: n - 1000,
                exp: n - 700,
                ..Claims::valid()
            })),
            "ticket_expired",
        ),
        (
            "autre passerelle",
            Some(key.gateway_ticket(&Claims {
                gw: "https://autre.example.com",
                ..Claims::valid()
            })),
            "ticket_wrong_audience",
        ),
        (
            "org non autorisée",
            Some(key.gateway_ticket(&Claims {
                org: "org_inconnue",
                ..Claims::valid()
            })),
            "ticket_wrong_org",
        ),
        (
            "op connect",
            Some(key.gateway_ticket(&Claims {
                op: "connect",
                ..Claims::valid()
            })),
            "ticket_wrong_operation",
        ),
        (
            "sct1 (CDN), domaine CDN",
            Some(key.sign_raw("sct1", b"synco-cdn-ticket\0", &Claims::valid().json())),
            "ticket_invalid",
        ),
        (
            "srt1 (runtime), domaine runtime",
            Some(key.sign_raw(
                "srt1",
                b"synco-ai-runtime-ticket\0",
                &Claims::valid().json(),
            )),
            "ticket_invalid",
        ),
        (
            "préfixe sgt1 mais domaine runtime",
            Some(key.sign_raw(
                "sgt1",
                b"synco-ai-runtime-ticket\0",
                &Claims::valid().json(),
            )),
            "ticket_invalid",
        ),
        (
            "préfixe sgt1 mais domaine CDN",
            Some(key.sign_raw("sgt1", b"synco-cdn-ticket\0", &Claims::valid().json())),
            "ticket_invalid",
        ),
        (
            "signé par une autre clé",
            Some(TestKey::new("k1", 8).gateway_ticket(&Claims::valid())),
            "ticket_invalid",
        ),
    ];
    // Même défauts sur /models.
    for (name, ticket, expected) in cases.drain(..) {
        let resp = gw.chat(ticket.as_deref(), CHAT_BODY).await;
        assert_eq!(
            code(resp).await,
            (401, expected.to_string()),
            "chat : {name}"
        );
        let mut req = gw.http.get(gw.url("/models"));
        if let Some(t) = &ticket {
            req = req.bearer_auth(t);
        }
        assert_eq!(
            code(req.send().await.expect("réponse")).await,
            (401, expected.to_string()),
            "models : {name}"
        );
    }
    assert_eq!(
        gw.ollama.calls(),
        0,
        "Ollama a été appelé par une requête non authentifiée"
    );
}

#[tokio::test]
async fn schema_d_authorization_incorrect_refuse() {
    let key = TestKey::new("k1", 7);
    let gw = start_gateway(&key, Options::default()).await;
    let t = key.gateway_ticket(&Claims::valid());
    for header in [t.clone(), format!("Basic {t}"), format!("bearer {t}")] {
        let resp = gw
            .http
            .post(gw.url("/v1/chat/completions"))
            .header("authorization", header)
            .body(CHAT_BODY)
            .send()
            .await
            .expect("réponse");
        assert_eq!(resp.status(), 401);
    }
    // Un ticket en paramètre d'URL n'est pas accepté (il finirait dans les journaux des proxys).
    let resp = gw
        .http
        .post(gw.url(&format!("/v1/chat/completions?ticket={t}")))
        .body(CHAT_BODY)
        .send()
        .await
        .expect("réponse");
    assert_eq!(resp.status(), 401);
    assert_eq!(gw.ollama.calls(), 0);
}

#[tokio::test]
async fn ticket_valide_relaye_en_flux_avec_corps_reecrit_et_sans_le_ticket() {
    let key = TestKey::new("k1", 7);
    let gw = start_gateway(&key, Options::default()).await;
    let ticket = key.gateway_ticket(&Claims::valid());

    let body = r#"{"model":"llama3.2","messages":[{"role":"user","content":"bonjour"}],"stream":true,"max_tokens":9999999,"n":40,"keep_alive":-1,"options":{"num_ctx":1000000}}"#;
    let resp = gw.chat(Some(&ticket), body).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers()["content-type"], "text/event-stream");
    let text = resp.text().await.expect("corps");
    assert!(text.contains("salut") && text.contains("[DONE]"));

    assert_eq!(gw.ollama.calls(), 1);
    let sent: serde_json::Value =
        serde_json::from_str(gw.ollama.spy.last_body.lock().unwrap().as_ref().unwrap()).unwrap();
    assert_eq!(sent["max_tokens"], 4096);
    assert_eq!(sent["n"], 1);
    assert_eq!(sent["model"], "llama3.2");
    assert!(sent.get("keep_alive").is_none() && sent.get("options").is_none());
    assert_eq!(
        gw.ollama.spy.paths.lock().unwrap().as_slice(),
        ["/v1/chat/completions"]
    );
    let headers = gw.ollama.spy.last_headers.lock().unwrap().clone().unwrap();
    assert!(
        headers.get("authorization").is_none(),
        "le ticket ne doit jamais atteindre Ollama"
    );
    assert!(!format!("{headers:?}").contains(&ticket));
}

#[tokio::test]
async fn modele_non_autorise_corps_invalide_et_corps_trop_gros_sans_appel_ollama() {
    let key = TestKey::new("k1", 7);
    let gw = start_gateway(&key, Options::default()).await;
    let t = key.gateway_ticket(&Claims::valid());

    assert_eq!(
        code(
            gw.chat(Some(&t), r#"{"model":"autre-modele","messages":[]}"#)
                .await
        )
        .await,
        (403, "model_not_allowed".into())
    );
    assert_eq!(
        code(gw.chat(Some(&t), "pas du json").await).await,
        (400, "invalid_json".into())
    );
    assert_eq!(
        code(gw.chat(Some(&t), r#"{"model":"llama3.2"}"#).await).await,
        (400, "invalid_body".into())
    );
    let big = format!(
        r#"{{"model":"llama3.2","messages":[{{"role":"user","content":"{}"}}]}}"#,
        "x".repeat(70 * 1024)
    );
    assert_eq!(
        code(gw.chat(Some(&t), &big).await).await,
        (413, "body_too_large".into())
    );
    assert_eq!(gw.ollama.calls(), 0);
}

#[tokio::test]
async fn seules_les_routes_prevues_existent() {
    let key = TestKey::new("k1", 7);
    let gw = start_gateway(&key, Options::default()).await;
    let t = key.gateway_ticket(&Claims::valid());

    let probes: [(&str, &str); 14] = [
        ("POST", "/api/pull"),
        ("DELETE", "/api/delete"),
        ("POST", "/api/delete"),
        ("POST", "/api/create"),
        ("GET", "/api/tags"),
        ("POST", "/api/generate"),
        ("POST", "/api/chat"),
        ("GET", "/v1/models"),
        ("POST", "/v1/embeddings"),
        ("POST", "/v1/completions"),
        ("POST", "/chat"),
        ("POST", "/chat/abc/tool-result"),
        ("GET", "/"),
        ("GET", "/v1/chat/completions"),
    ];
    for (method, path) in probes {
        for with_ticket in [false, true] {
            let mut req = gw
                .http
                .request(method.parse().unwrap(), gw.url(path))
                .body("{}");
            if with_ticket {
                req = req.bearer_auth(&t);
            }
            let resp = req.send().await.expect("réponse");
            assert_eq!(
                resp.status(),
                404,
                "{method} {path} (ticket : {with_ticket})"
            );
        }
    }
    // Mauvaise méthode sur les routes connues : 404 aussi, jamais 405 ni relais.
    for (method, path) in [
        ("POST", "/models"),
        ("DELETE", "/models"),
        ("PUT", "/v1/chat/completions"),
        ("POST", "/health"),
    ] {
        let resp = gw
            .http
            .request(method.parse().unwrap(), gw.url(path))
            .bearer_auth(&t)
            .body("{}")
            .send()
            .await
            .expect("réponse");
        assert_eq!(resp.status(), 404, "{method} {path}");
    }
    assert_eq!(gw.ollama.calls(), 0);
}

#[tokio::test]
async fn health_sans_ticket_et_models_filtres_avec_ticket() {
    let key = TestKey::new("k1", 7);
    let gw = start_gateway(&key, Options::default()).await;
    let resp = gw
        .http
        .get(gw.url("/health"))
        .send()
        .await
        .expect("réponse");
    assert_eq!(
        (resp.status().as_u16(), resp.text().await.unwrap().as_str()),
        (200, "ok")
    );
    assert_eq!(gw.ollama.calls(), 0, "/health ne touche pas Ollama");

    let t = key.gateway_ticket(&Claims::valid());
    let v: serde_json::Value = gw
        .http
        .get(gw.url("/models"))
        .bearer_auth(&t)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(v["models"], serde_json::json!(["llama3.2"]));

    let all = start_gateway(
        &key,
        Options {
            allowed_models: vec![],
            ..Options::default()
        },
    )
    .await;
    let t = key.gateway_ticket(&Claims::valid());
    let v: serde_json::Value = all
        .http
        .get(all.url("/models"))
        .bearer_auth(&t)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(v["models"], serde_json::json!(["llama3.2", "autre-modele"]));
}

#[tokio::test]
async fn cors_limite_a_l_origine_de_l_app_et_runtime_sans_cors() {
    let key = TestKey::new("k1", 7);
    let gw = start_gateway(&key, Options::default()).await;

    let preflight = |origin: &'static str| {
        gw.http
            .request(reqwest::Method::OPTIONS, gw.url("/v1/chat/completions"))
            .header("origin", origin)
            .header("access-control-request-method", "POST")
            .header(
                "access-control-request-headers",
                "authorization,content-type",
            )
            .send()
    };
    let ok = preflight("https://app.example.com").await.unwrap();
    assert_eq!(
        ok.headers()["access-control-allow-origin"],
        "https://app.example.com"
    );
    let bad = preflight("https://evil.example.org").await.unwrap();
    assert!(bad.headers().get("access-control-allow-origin").is_none());

    // Le runtime appelle sans en-tête Origin : accepté (il n'y a rien à vérifier côté CORS).
    let t = key.gateway_ticket(&Claims::valid());
    assert_eq!(gw.chat(Some(&t), CHAT_BODY).await.status(), 200);

    // Sans ALLOWED_ORIGIN : aucun en-tête CORS du tout.
    let closed = start_gateway(
        &key,
        Options {
            allowed_origin: None,
            ..Options::default()
        },
    )
    .await;
    let resp = closed
        .http
        .get(closed.url("/health"))
        .header("origin", "https://app.example.com")
        .send()
        .await
        .unwrap();
    assert!(resp.headers().get("access-control-allow-origin").is_none());
}

#[tokio::test]
async fn rotation_par_kid_avec_relecture_bornee() {
    let old = TestKey::new("ai-1", 1);
    let new = TestKey::new("ai-2", 2);
    let opts = Options {
        keys: KeyStoreConfig {
            unknown_kid_min_interval: Duration::from_millis(400),
            ..KeyStoreConfig::default()
        },
        ..Options::default()
    };
    let gw = start_gateway(&old, opts).await;
    assert_eq!(gw.api.hits(), 1, "lecture au démarrage");

    let t_old = old.gateway_ticket(&Claims::valid());
    assert_eq!(gw.chat(Some(&t_old), CHAT_BODY).await.status(), 200);
    assert_eq!(gw.api.hits(), 1, "kid connu : aucune relecture");

    // Nouveau kid pas encore publié : une relecture, puis refus ; les suivantes sont bornées.
    let t_new = new.gateway_ticket(&Claims::valid());
    for _ in 0..5 {
        assert_eq!(
            code(gw.chat(Some(&t_new), CHAT_BODY).await).await,
            (401, "ticket_unknown_key".into())
        );
    }
    assert_eq!(gw.api.hits(), 2, "une seule relecture malgré 5 requêtes");

    // L'API publie les deux clés ; après l'intervalle, la relecture suivante accepte le ticket.
    gw.api.publish(&[&old, &new]);
    tokio::time::sleep(Duration::from_millis(450)).await;
    assert_eq!(gw.chat(Some(&t_new), CHAT_BODY).await.status(), 200);
    assert_eq!(gw.chat(Some(&t_old), CHAT_BODY).await.status(), 200);
    assert_eq!(gw.api.hits(), 3);

    // Retrait de l'ancienne clé : refusée dès la relecture périodique.
    gw.api.publish(&[&new]);
    assert!(gw.keys.refresh().await);
    assert_eq!(
        code(gw.chat(Some(&t_old), CHAT_BODY).await).await,
        (401, "ticket_unknown_key".into())
    );
    assert_eq!(gw.chat(Some(&t_new), CHAT_BODY).await.status(), 200);
}

#[tokio::test]
async fn echec_ferme_reponse_invalide_et_cache_perime() {
    let key = TestKey::new("k1", 7);
    let opts = Options {
        keys: KeyStoreConfig {
            max_stale: Duration::from_millis(500),
            unknown_kid_min_interval: Duration::from_secs(60),
            ..KeyStoreConfig::default()
        },
        ..Options::default()
    };
    let gw = start_gateway(&key, opts).await;
    let t = key.gateway_ticket(&Claims::valid());
    assert_eq!(gw.chat(Some(&t), CHAT_BODY).await.status(), 200);

    // Réponses invalides : le cache n'est pas écrasé.
    for (status, body) in [
        (500, "boom"),
        (200, "pas json"),
        (200, r#"{"v":1,"keys":{}}"#),
        (200, r#"{"v":1,"keys":{"k1":"court"}}"#),
    ] {
        gw.api.set_raw(status, body);
        assert!(!gw.keys.refresh().await);
        assert_eq!(
            gw.chat(Some(&t), CHAT_BODY).await.status(),
            200,
            "cache conservé après {status} {body}"
        );
    }

    // L'API reste en échec : une fois le cache périmé, plus aucun ticket n'est accepté.
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(
        code(gw.chat(Some(&t), CHAT_BODY).await).await,
        (401, "ticket_unknown_key".into())
    );
    // Retour de l'API : la relecture périodique rétablit le service.
    gw.api.publish(&[&key]);
    assert!(gw.keys.refresh().await);
    assert_eq!(gw.chat(Some(&t), CHAT_BODY).await.status(), 200);
}

#[tokio::test]
async fn demarrage_sans_cles_lisibles_refuse_tout() {
    let key = TestKey::new("k1", 7);
    let gw = common::start_gateway_with_unreadable_api(&key).await;
    let t = key.gateway_ticket(&Claims::valid());
    assert_eq!(
        code(gw.chat(Some(&t), CHAT_BODY).await).await,
        (401, "ticket_unknown_key".into())
    );
    assert_eq!(gw.ollama.calls(), 0);
}

#[tokio::test]
async fn liste_d_orgs_vide_n_autorise_personne() {
    let key = TestKey::new("k1", 7);
    let gw = start_gateway(
        &key,
        Options {
            allowed_orgs: vec![],
            ..Options::default()
        },
    )
    .await;
    let t = key.gateway_ticket(&Claims::valid());
    assert_eq!(
        code(gw.chat(Some(&t), CHAT_BODY).await).await,
        (401, "ticket_wrong_org".into())
    );
    assert_eq!(gw.ollama.calls(), 0);
}

#[tokio::test]
async fn erreur_d_ollama_non_relayee() {
    let key = TestKey::new("k1", 7);
    let gw = start_gateway(&key, Options::default()).await;
    *gw.ollama.spy.mode.lock().unwrap() = Mode::ModelNotFound;
    let t = key.gateway_ticket(&Claims::valid());
    let resp = gw.chat(Some(&t), CHAT_BODY).await;
    assert_eq!(resp.status(), 502);
    let text = resp.text().await.unwrap();
    assert_eq!(text, r#"{"error":"upstream_error"}"#);
    assert!(!text.contains("secret-name"));
}

async fn wait_active(gw: &Gateway, expected: usize) -> bool {
    for _ in 0..100 {
        if gw
            .ollama
            .spy
            .active
            .load(std::sync::atomic::Ordering::SeqCst)
            == expected
        {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

/// Ouvre un flux sans fin et le garde ouvert ; rend la réponse (la place reste prise).
async fn open_stream(gw: &Gateway, ticket: &str) -> reqwest::Response {
    let resp = gw.chat(Some(ticket), CHAT_BODY).await;
    assert_eq!(resp.status(), 200);
    resp
}

#[tokio::test]
async fn limite_par_utilisateur_429_sans_appel_ollama_puis_liberation_a_la_deconnexion() {
    let key = TestKey::new("k1", 7);
    let gw = start_gateway(
        &key,
        Options {
            max_per_user: 2,
            max_concurrent: 10,
            ..Options::default()
        },
    )
    .await;
    *gw.ollama.spy.mode.lock().unwrap() = Mode::Endless;
    let t = key.gateway_ticket(&Claims::valid());

    let s1 = open_stream(&gw, &t).await;
    let _s2 = open_stream(&gw, &t).await;
    assert!(wait_active(&gw, 2).await);
    let calls = gw.ollama.calls();

    let resp = gw.chat(Some(&t), CHAT_BODY).await;
    assert_eq!(resp.status(), 429);
    assert_eq!(resp.headers()["retry-after"], "1");
    assert_eq!(
        resp.json::<serde_json::Value>().await.unwrap()["error"],
        "too_many_requests_user"
    );
    assert_eq!(
        gw.ollama.calls(),
        calls,
        "Ollama ne doit pas être appelé au-delà de la limite"
    );

    // Un autre utilisateur n'est pas touché (limite par `sub`).
    let other = key.sign_raw(
        "sgt1",
        b"synco-ai-gateway-ticket\0",
        &Claims::valid().json().replace(SUB, "autre_user"),
    );
    let _s3 = open_stream(&gw, &other).await;

    // Déconnexion du client en plein flux : le flux d'Ollama est abandonné et la place rendue.
    drop(s1);
    assert!(
        wait_active(&gw, 2).await,
        "le flux amont du client parti doit être abandonné"
    );
    assert_eq!(gw.chat(Some(&t), CHAT_BODY).await.status(), 200);
}

#[tokio::test]
async fn limite_globale_429() {
    let key = TestKey::new("k1", 7);
    let gw = start_gateway(
        &key,
        Options {
            max_per_user: 5,
            max_concurrent: 2,
            ..Options::default()
        },
    )
    .await;
    *gw.ollama.spy.mode.lock().unwrap() = Mode::Endless;
    let t1 = key.gateway_ticket(&Claims::valid());
    let t2 = key.sign_raw(
        "sgt1",
        b"synco-ai-gateway-ticket\0",
        &Claims::valid().json().replace(SUB, "u2"),
    );
    let t3 = key.sign_raw(
        "sgt1",
        b"synco-ai-gateway-ticket\0",
        &Claims::valid().json().replace(SUB, "u3"),
    );
    let _a = open_stream(&gw, &t1).await;
    let _b = open_stream(&gw, &t2).await;
    let calls = gw.ollama.calls();
    assert_eq!(
        code(gw.chat(Some(&t3), CHAT_BODY).await).await,
        (429, "too_many_requests".into())
    );
    assert_eq!(gw.ollama.calls(), calls);
}

#[tokio::test]
async fn place_rendue_a_la_fin_du_flux() {
    let key = TestKey::new("k1", 7);
    let gw = start_gateway(
        &key,
        Options {
            max_per_user: 1,
            ..Options::default()
        },
    )
    .await;
    let t = key.gateway_ticket(&Claims::valid());
    for _ in 0..3 {
        let resp = gw.chat(Some(&t), CHAT_BODY).await;
        assert_eq!(resp.status(), 200);
        resp.text().await.unwrap();
    }
}

#[tokio::test]
async fn deconnexion_avant_la_reponse_d_ollama_abandonne_la_requete_amont() {
    let key = TestKey::new("k1", 7);
    let gw = start_gateway(
        &key,
        Options {
            max_per_user: 1,
            ..Options::default()
        },
    )
    .await;
    *gw.ollama.spy.mode.lock().unwrap() = Mode::SlowHeaders(5000);
    let t = key.gateway_ticket(&Claims::valid());

    let impatient = reqwest::Client::builder()
        .timeout(Duration::from_millis(300))
        .build()
        .unwrap();
    let res = impatient
        .post(gw.url("/v1/chat/completions"))
        .bearer_auth(&t)
        .header("content-type", "application/json")
        .body(CHAT_BODY)
        .send()
        .await;
    assert!(res.is_err(), "le client doit avoir abandonné");
    assert!(
        wait_active(&gw, 0).await,
        "la requête vers Ollama doit être abandonnée à la déconnexion du client"
    );

    // La place est rendue : une nouvelle requête passe (limite par utilisateur = 1).
    *gw.ollama.spy.mode.lock().unwrap() = Mode::Normal;
    assert_eq!(gw.chat(Some(&t), CHAT_BODY).await.status(), 200);
}

#[tokio::test]
async fn relecture_des_cles_serialisee_une_lecture_ancienne_n_ecrase_pas_une_recente() {
    let old = TestKey::new("ai-1", 1);
    let new = TestKey::new("ai-2", 2);
    let gw = start_gateway(&old, Options::default()).await;
    // Lecture A : voit l'ancien jeu mais répond lentement.
    gw.api
        .delay_ms
        .store(400, std::sync::atomic::Ordering::SeqCst);
    let keys_a = gw.keys.clone();
    let a = tokio::spawn(async move { keys_a.refresh().await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    // Lecture B : lancée après la publication d'une rotation, répond vite.
    gw.api.publish(&[&new]);
    gw.api
        .delay_ms
        .store(0, std::sync::atomic::Ordering::SeqCst);
    assert!(gw.keys.refresh().await);
    assert!(a.await.unwrap());
    let current = gw.keys.current();
    assert!(
        current.contains_key("ai-2") && !current.contains_key("ai-1"),
        "le jeu le plus récent doit l'emporter"
    );
}
