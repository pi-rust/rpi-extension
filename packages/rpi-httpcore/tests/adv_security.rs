//! Adversarial HTTP security probes for rpi-httpcore (2026-11-23 review).
//! login rate limiting, marker/CSRF enforcement, Host/Origin policy,
//! session-cookie scoping. Security layer is exposed `pub` for these probes.
use axum::body::Body;
use axum::http::{Request, StatusCode};
use rpi_httpcore::security::Security;
use rpi_httpcore::transport::origin::HostOriginPolicy;
use std::sync::Arc;
use tower::ServiceExt;

fn app(security: Arc<Security>) -> axum::Router {
    use axum::middleware::from_fn_with_state;
    use axum::routing::{get, post};
    use rpi_httpcore::security::guard;

    async fn login(
        axum::extract::State(security): axum::extract::State<Arc<Security>>,
        headers: axum::http::HeaderMap,
        axum::Json(body): axum::Json<serde_json::Value>,
    ) -> axum::response::Response {
        let password = body
            .get("password")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        rpi_httpcore::security::login_for(
            &security,
            &headers,
            rpi_httpcore::security::LoginRequest { password },
        )
        .await
    }

    axum::Router::new()
        .route("/api/v1/secret", get(|| async { "TOP-SECRET" }))
        .route("/api/v1/auth", post(login))
        .layer(from_fn_with_state(security.clone(), guard))
        .with_state(security)
}

#[tokio::test]
async fn adv_brute_force_is_rate_limited() {
    let security = Arc::new(Security::new(
        ["127.0.0.1:0".to_string()],
        Some("correct-horse"),
    ));
    let app = app(security.clone());
    let mut limited = false;
    for i in 0..15 {
        let req = Request::builder()
            .method("POST")
            .uri("/api/v1/auth")
            .header("host", "127.0.0.1:0")
            .header("x-rpi-web", "1")
            .header("content-type", "application/json")
            .body(Body::from("{\"password\":\"wrong\"}"))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        if resp.status() == StatusCode::TOO_MANY_REQUESTS {
            limited = true;
            println!("rate limited at attempt {i}");
            break;
        }
    }
    assert!(limited, "15 wrong passwords must trigger rate limiting");
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/auth")
        .header("host", "127.0.0.1:0")
        .header("x-rpi-web", "1")
        .header("content-type", "application/json")
        .body(Body::from("{\"password\":\"correct-horse\"}"))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn adv_missing_marker_is_403_with_security_headers() {
    let security = Arc::new(Security::new(["127.0.0.1:0".to_string()], Some("pw")));
    let app = app(security);
    // GET needs no mutation marker but still needs a lease -> 401.
    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/secret")
        .header("host", "127.0.0.1:0")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    // A mutating POST without the x-rpi-web marker -> 403 (CSRF).
    let req2 = Request::builder()
        .method("POST")
        .uri("/api/v1/secret")
        .header("host", "127.0.0.1:0")
        .body(Body::empty())
        .unwrap();
    let resp2 = app.oneshot(req2).await.unwrap();
    assert_eq!(resp2.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        resp.headers()
            .get("x-frame-options")
            .and_then(|v| v.to_str().ok()),
        Some("DENY")
    );
}

#[tokio::test]
async fn adv_wrong_host_is_403() {
    let security = Arc::new(Security::new(["127.0.0.1:0".to_string()], Some("pw")));
    let app = app(security);
    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/secret")
        .header("host", "evil.example.com")
        .header("x-rpi-web", "1")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn adv_cross_origin_is_rejected() {
    let policy = HostOriginPolicy::new(["127.0.0.1:0".to_string()]);
    let mut h = axum::http::HeaderMap::new();
    h.insert("host", "127.0.0.1:0".parse().unwrap());
    h.insert("origin", "http://evil.example.com".parse().unwrap());
    assert!(!policy.origin_allowed(&h));
    let mut h2 = axum::http::HeaderMap::new();
    h2.insert("host", "127.0.0.1:0".parse().unwrap());
    h2.insert("origin", "http://127.0.0.1:0".parse().unwrap());
    assert!(policy.origin_allowed(&h2));
    let mut h3 = axum::http::HeaderMap::new();
    h3.insert("host", "127.0.0.1:0".parse().unwrap());
    assert!(policy.origin_allowed(&h3), "no origin = same-origin GET");
}

#[tokio::test]
async fn adv_login_issues_httponly_samesite_cookie_that_authorizes() {
    let security = Arc::new(Security::new(["127.0.0.1:0".to_string()], Some("pw")));
    let app = app(security.clone());
    let app2 = app.clone();
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/auth")
        .header("host", "127.0.0.1:0")
        .header("x-rpi-web", "1")
        .header("content-type", "application/json")
        .body(Body::from("{\"password\":\"pw\"}"))
        .unwrap();
    let resp = app2.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let cookie = resp
        .headers()
        .get("set-cookie")
        .expect("session cookie must be set")
        .to_str()
        .unwrap()
        .to_string();
    let lower = cookie.to_lowercase();
    assert!(lower.contains("httponly"), "{cookie}");
    assert!(lower.contains("samesite"), "{cookie}");
    let token = cookie.split(';').next().unwrap().to_string();
    let req2 = Request::builder()
        .method("GET")
        .uri("/api/v1/secret")
        .header("host", "127.0.0.1:0")
        .header("cookie", token)
        .body(Body::empty())
        .unwrap();
    let resp2 = app.oneshot(req2).await.unwrap();
    assert_eq!(resp2.status(), StatusCode::OK);
}
