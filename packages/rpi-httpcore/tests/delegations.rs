//! Ephemeral authenticated HTTP listener; never starts native or reads real history.
use rpi_extcore::service::{BackendConfig, Service};
use serde_json::json;
use std::time::Duration;
#[tokio::test]
async fn delegation_routes_preserve_auth_origin_marker_and_strict_cursors() {
    let root = std::env::temp_dir().join(format!("rpi-message-http-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let core = Service::new(BackendConfig {
        project: root.clone(),
        registry: root.join("sessions.json"),
        rpi_bin: root.join("must-not-start.exe"),
        ..Default::default()
    })
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let config = rpi_httpcore::AppConfig {
        port: address.port(),
        allowed_hosts: Some(vec![address.to_string()]),
        ..Default::default()
    };
    let (shutdown, receiver) = tokio::sync::watch::channel(false);
    // A single test in this executable keeps the temporary env override isolated.
    let previous = std::env::var_os("RPI_HTTP_PASSWORD");
    let password = uuid::Uuid::new_v4().to_string();
    std::env::set_var("RPI_HTTP_PASSWORD", &password);
    let app = rpi_httpcore::router(core.clone(), &config, receiver.clone());
    match previous {
        Some(value) => std::env::set_var("RPI_HTTP_PASSWORD", value),
        None => std::env::remove_var("RPI_HTTP_PASSWORD"),
    }
    let server = tokio::spawn(async move {
        let mut receiver = receiver;
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = receiver.changed().await;
            })
            .await
            .unwrap();
    });
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let base = format!("http://{address}");
    let roster = format!("{base}/api/v1/sessions/missing/delegations");
    let events = format!("{roster}/child/events?epoch=epoch&afterSeq=0");
    let query = format!("{roster}/child/events/query");
    let cancel = format!("{roster}/child/cancel");
    for url in [&roster, &events] {
        assert_eq!(client.get(url).send().await.unwrap().status(), 401);
    }
    assert_eq!(
        client
            .post(&cancel)
            .json(&json!({"epoch":"epoch"}))
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    let login = client
        .post(format!("{base}/api/v1/auth"))
        .header("X-Rpi-Web", "1")
        .json(&json!({"password":password}))
        .send()
        .await
        .unwrap();
    assert_eq!(login.status(), 200);
    let cookie = login.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    for url in [&roster, &events] {
        for (header, value) in [
            ("Host", "untrusted.invalid"),
            ("Origin", "https://untrusted.invalid"),
        ] {
            assert_eq!(
                client
                    .get(url)
                    .header("cookie", &cookie)
                    .header(header, value)
                    .send()
                    .await
                    .unwrap()
                    .status(),
                403
            );
        }
        assert_eq!(
            client
                .get(url)
                .header("cookie", &cookie)
                .send()
                .await
                .unwrap()
                .status(),
            404
        );
    }
    for body in [
        json!({"epoch":"epoch","afterSeq":0,"limit":0}),
        json!({"epoch":"epoch","afterSeq":0,"limit":33}),
        json!({"epoch":"epoch","afterSeq":0,"limit":32,"parentSessionId":"other"}),
    ] {
        assert_eq!(
            client
                .post(&query)
                .header("cookie", &cookie)
                .header("X-Rpi-Web", "1")
                .json(&body)
                .send()
                .await
                .unwrap()
                .status(),
            400
        );
    }
    assert_eq!(
        client
            .post(&cancel)
            .header("cookie", &cookie)
            .header("X-Rpi-Web", "1")
            .json(&json!({"epoch":"epoch","command":"prompt"}))
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    assert_eq!(
        client
            .post(&cancel)
            .header("cookie", &cookie)
            .header("X-Rpi-Web", "1")
            .json(&json!({"epoch":"epoch"}))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    assert!(core.live_session("missing").await.is_none());
    let _ = shutdown.send(true);
    server.await.unwrap();
    core.shutdown().await;
    std::fs::remove_dir_all(root).unwrap();
}
