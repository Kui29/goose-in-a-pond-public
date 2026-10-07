use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use aws_lc_rs::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_FIXED_SIGNING};
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use base64::{engine::general_purpose::STANDARD, Engine};
use http_body_util::BodyExt;
use pondcredentials::{client_of, router, AppState, Client, Config, Issuer, RateLimiter};
use tower::ServiceExt;

/// A fresh P-256 key as a .p8, and its public half. Made per test, so nothing secret is committed.
fn key() -> (String, Vec<u8>) {
    let pair = EcdsaKeyPair::generate(&ECDSA_P256_SHA256_FIXED_SIGNING).unwrap();
    let der = pair.to_pkcs8v1().unwrap().as_ref().to_vec();
    let body = STANDARD.encode(&der);
    let lines: Vec<&str> = body
        .as_bytes()
        .chunks(64)
        .map(|c| std::str::from_utf8(c).unwrap())
        .collect();
    let pem = format!(
        "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
        lines.join("\n")
    );
    (pem, pair.public_key().as_ref().to_vec())
}

fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    move |name| map.get(name).cloned()
}

fn base() -> Vec<(&'static str, &'static str)> {
    vec![
        ("APPLE_TEAM_ID", "TEAMID1234"),
        ("APPLE_KEY_ID", "KEYID12345"),
        ("APPLE_PRIVATE_KEY_FILE", "/run/secrets/key.p8"),
    ]
}

fn config_with(extra: &[(&str, &str)], pem: &str) -> Result<Config, String> {
    let mut pairs = base();
    pairs.extend_from_slice(extra);
    let pem = pem.to_string();
    Config::from_env(env(&pairs), move |_| Ok(pem.clone()))
}

fn verify(token: &str, public: &[u8]) -> serde_json::Value {
    let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::ES256);
    validation.validate_exp = false;
    validation.required_spec_claims.clear();
    jsonwebtoken::decode::<serde_json::Value>(
        token,
        &jsonwebtoken::DecodingKey::from_ec_der(public),
        &validation,
    )
    .expect("the token verifies against the key's public half")
    .claims
}

// ── Configuration ────────────────────────────────────────────

#[test]
fn a_complete_environment_gives_the_defaults() {
    let (pem, _) = key();
    let c = config_with(&[], &pem).unwrap();
    assert_eq!(c.ttl, Duration::from_secs(30 * 86_400));
    assert_eq!(c.refresh_after, Duration::from_secs(24 * 3600));
    assert_eq!(c.rate_per_minute, 20);
    assert!(
        !c.trust_proxy,
        "a forwarded header is only a claim unless we are told otherwise"
    );
}

#[test]
fn a_missing_setting_is_named() {
    let (pem, _) = key();
    for missing in ["APPLE_TEAM_ID", "APPLE_KEY_ID", "APPLE_PRIVATE_KEY_FILE"] {
        let pairs: Vec<_> = base().into_iter().filter(|(k, _)| *k != missing).collect();
        let pem = pem.clone();
        let err = Config::from_env(env(&pairs), move |_| Ok(pem.clone()))
            .err()
            .unwrap_or_else(|| panic!("{missing} was accepted missing"));
        assert!(err.contains(missing), "{err}");
    }
}

#[test]
fn ids_must_be_the_ten_characters_apple_issues() {
    let (pem, _) = key();
    for (name, bad) in [("APPLE_TEAM_ID", "SHORT"), ("APPLE_KEY_ID", "has space!!")] {
        let mut pairs: Vec<_> = base().into_iter().filter(|(k, _)| *k != name).collect();
        pairs.push((name, bad));
        let pem = pem.clone();
        let err = Config::from_env(env(&pairs), move |_| Ok(pem.clone()))
            .err()
            .unwrap();
        assert!(err.contains(name) && err.contains("10"), "{err}");
    }
}

#[test]
fn a_lifetime_past_apples_ceiling_is_refused_at_startup() {
    let (pem, _) = key();
    assert!(config_with(&[("TOKEN_TTL_DAYS", "182")], &pem).is_ok());
    for bad in ["0", "183", "365"] {
        let err = config_with(&[("TOKEN_TTL_DAYS", bad)], &pem).err().unwrap();
        assert!(err.contains("TOKEN_TTL_DAYS"), "{bad}: {err}");
    }
}

#[test]
fn numbers_must_be_numbers_and_limits_must_leave_room_to_work() {
    let (pem, _) = key();
    for (name, bad) in [
        ("RATE_LIMIT_PER_MINUTE", "lots"),
        ("RATE_LIMIT_PER_MINUTE", "0"),
        ("TOKEN_REFRESH_HOURS", "0"),
    ] {
        let err = config_with(&[(name, bad)], &pem).err().unwrap();
        assert!(err.contains(name), "{name}={bad}: {err}");
    }
}

#[test]
fn an_unreadable_key_file_says_so() {
    let err = Config::from_env(env(&base()), |_| Err("no such file".into()))
        .err()
        .unwrap();
    assert!(
        err.contains("APPLE_PRIVATE_KEY_FILE") && err.contains("no such file"),
        "{err}"
    );
}

// ── Issuing ──────────────────────────────────────────────────

#[test]
fn a_key_that_cannot_sign_stops_the_service_starting() {
    let cfg = config_with(&[], "definitely not a key").unwrap();
    assert!(Issuer::new(&cfg, 1_790_000_000).is_err());
}

#[test]
fn a_token_verifies_and_lives_for_the_configured_time() {
    let (pem, public) = key();
    let cfg = config_with(&[("TOKEN_TTL_DAYS", "10")], &pem).unwrap();
    let issuer = Issuer::new(&cfg, 1_790_000_000).unwrap();

    let issued = issuer.token(1_790_000_000).unwrap();
    let claims = verify(&issued.token, &public);

    assert_eq!(claims["iss"], "TEAMID1234");
    assert_eq!(claims["exp"], issued.expires_at);
    assert_eq!(issued.expires_at - issued.issued_at, 10 * 86_400);
    assert_eq!(
        jsonwebtoken::decode_header(&issued.token)
            .unwrap()
            .kid
            .as_deref(),
        Some("KEYID12345")
    );
}

#[test]
fn one_signature_serves_a_day_of_requests_then_a_fresh_one() {
    let (pem, _) = key();
    let cfg = config_with(&[], &pem).unwrap();
    let issuer = Issuer::new(&cfg, 1_790_000_000).unwrap();

    let first = issuer.token(1_790_000_000).unwrap();
    let later_same_day = issuer.token(1_790_000_000 + 23 * 3600).unwrap();
    assert_eq!(
        first.token, later_same_day.token,
        "reused inside the refresh window"
    );

    let next_day = issuer.token(1_790_000_000 + 25 * 3600).unwrap();
    assert_ne!(first.token, next_day.token);
    assert!(next_day.expires_at > first.expires_at);
}

#[test]
fn a_clock_that_steps_backwards_never_serves_a_token_from_the_future() {
    let (pem, _) = key();
    let cfg = config_with(&[], &pem).unwrap();
    let issuer = Issuer::new(&cfg, 1_790_000_000).unwrap();
    issuer.token(1_790_000_000).unwrap();

    let earlier = issuer.token(1_780_000_000).unwrap();
    assert_eq!(earlier.issued_at, 1_780_000_000);
}

// ── Rate limiting ────────────────────────────────────────────

fn v4(a: u8) -> Client {
    Client::V4([10, 0, 0, a])
}

#[test]
fn a_client_gets_its_allowance_and_is_told_when_to_return() {
    let limiter = RateLimiter::new(3, Duration::from_secs(60), 100);
    let t0 = Instant::now();
    for _ in 0..3 {
        assert!(limiter.check(&v4(1), t0).is_ok());
    }
    let wait = limiter
        .check(&v4(1), t0 + Duration::from_secs(10))
        .unwrap_err();
    assert!(
        (49..=50).contains(&wait),
        "waits for the oldest hit to leave: {wait}"
    );
}

#[test]
fn the_window_slides_and_clients_are_independent() {
    let limiter = RateLimiter::new(1, Duration::from_secs(60), 100);
    let t0 = Instant::now();
    assert!(limiter.check(&v4(1), t0).is_ok());
    assert!(limiter.check(&v4(1), t0 + Duration::from_secs(30)).is_err());
    assert!(
        limiter.check(&v4(2), t0 + Duration::from_secs(30)).is_ok(),
        "another client"
    );
    assert!(
        limiter.check(&v4(1), t0 + Duration::from_secs(61)).is_ok(),
        "the window moved on"
    );
}

#[test]
fn addresses_inside_one_ipv6_block_share_one_allowance() {
    let a: std::net::IpAddr = "2001:db8:1:2::1".parse().unwrap();
    let b: std::net::IpAddr = "2001:db8:1:2:ffff:ffff:ffff:ffff".parse().unwrap();
    let other: std::net::IpAddr = "2001:db8:1:3::1".parse().unwrap();
    assert_eq!(
        Client::of(a),
        Client::of(b),
        "rotating within a /64 must not dodge the limit"
    );
    assert_ne!(Client::of(a), Client::of(other));
}

#[test]
fn a_flood_of_new_addresses_cannot_grow_memory_without_bound() {
    let limiter = RateLimiter::new(1, Duration::from_secs(60), 3);
    let t0 = Instant::now();
    for i in 1..=3 {
        assert!(limiter.check(&v4(i), t0).is_ok());
    }
    // Full of live entries: a newcomer is refused rather than evicting or growing.
    assert!(limiter.check(&v4(9), t0 + Duration::from_secs(1)).is_err());
    // Once the old ones age out there is room again.
    assert!(limiter.check(&v4(9), t0 + Duration::from_secs(120)).is_ok());
}

#[test]
fn behind_a_trusted_proxy_the_address_is_the_last_one_the_proxy_wrote() {
    let peer: SocketAddr = "172.18.0.2:5000".parse().unwrap();
    let mut headers = axum::http::HeaderMap::new();
    headers.insert("x-forwarded-for", "6.6.6.6, 203.0.113.9".parse().unwrap());

    assert_eq!(
        client_of(&headers, peer, true),
        Client::V4([203, 0, 113, 9]),
        "a forged first entry is ignored"
    );
    assert_eq!(
        client_of(&headers, peer, false),
        Client::V4([172, 18, 0, 2]),
        "untrusted: the header is a claim"
    );

    headers.insert("x-forwarded-for", "not an address".parse().unwrap());
    assert_eq!(
        client_of(&headers, peer, true),
        Client::V4([172, 18, 0, 2]),
        "unparseable falls back to the peer"
    );
}

// ── The service, over HTTP ───────────────────────────────────

struct Service {
    app: axum::Router,
    state: Arc<AppState>,
    public: Vec<u8>,
}

fn service(extra: &[(&str, &str)]) -> Service {
    let (pem, public) = key();
    let cfg = config_with(extra, &pem).unwrap();
    let state = Arc::new(AppState::new(&cfg).unwrap());
    Service {
        app: router(state.clone()),
        state,
        public,
    }
}

async fn send(
    svc: &Service,
    method: &str,
    uri: &str,
    peer: &str,
    xff: Option<&str>,
    body: Body,
) -> (StatusCode, axum::http::HeaderMap, String) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(x) = xff {
        builder = builder.header("x-forwarded-for", x);
    }
    let mut req = builder.body(body).unwrap();
    req.extensions_mut()
        .insert(ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
    let resp = svc.app.clone().oneshot(req).await.unwrap();
    let (status, headers) = (resp.status(), resp.headers().clone());
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        headers,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

const TOKEN: &str = "/v1/musickit/developer-token";

#[tokio::test]
async fn a_household_gets_a_token_that_verifies_and_is_told_not_to_keep_it_around() {
    let svc = service(&[]);
    let (status, headers, body) =
        send(&svc, "POST", TOKEN, "203.0.113.5:4000", None, Body::empty()).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(headers["cache-control"], "no-store");
    assert_eq!(headers["x-content-type-options"], "nosniff");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    let claims = verify(json["token"].as_str().unwrap(), &svc.public);
    assert_eq!(claims["exp"], json["expires_at"]);
}

#[tokio::test]
async fn nothing_the_service_says_can_contain_key_material() {
    let svc = service(&[]);
    for (method, uri) in [
        ("POST", TOKEN),
        ("GET", TOKEN),
        ("GET", "/healthz"),
        ("GET", "/nope"),
    ] {
        let (_, _, body) = send(&svc, method, uri, "203.0.113.5:4000", None, Body::empty()).await;
        assert!(
            !body.contains("PRIVATE KEY") && !body.contains("BEGIN"),
            "{method} {uri}: {body}"
        );
    }
}

#[tokio::test]
async fn only_a_post_asks_for_a_token() {
    let svc = service(&[]);
    let (status, _, _) = send(&svc, "GET", TOKEN, "203.0.113.5:4000", None, Body::empty()).await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn unknown_paths_are_a_plain_404() {
    let svc = service(&[]);
    let (status, _, body) = send(
        &svc,
        "GET",
        "/admin",
        "203.0.113.5:4000",
        None,
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.contains("Not found"));
}

#[tokio::test]
async fn a_request_body_of_any_size_is_refused() {
    let svc = service(&[]);
    let big = Body::from(vec![b'x'; 4096]);
    let (status, _, _) = send(&svc, "POST", TOKEN, "203.0.113.5:4000", None, big).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn health_counts_what_was_issued_and_what_was_refused_and_nothing_else() {
    let svc = service(&[("RATE_LIMIT_PER_MINUTE", "2")]);
    for _ in 0..4 {
        send(&svc, "POST", TOKEN, "203.0.113.5:4000", None, Body::empty()).await;
    }
    let (status, _, body) = send(&svc, "GET", "/healthz", "127.0.0.1:1", None, Body::empty()).await;
    assert_eq!(status, StatusCode::OK);
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        (json["issued"].as_u64(), json["limited"].as_u64()),
        (Some(2), Some(2))
    );
    assert_eq!(
        json.as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>(),
        ["issued", "limited", "ok", "uber_relayed", "uptime_s"]
            .map(String::from)
            .into_iter()
            .collect(),
        "aggregate counters only: no address, no key id"
    );
}

#[tokio::test]
async fn the_limit_holds_and_says_when_to_come_back() {
    let svc = service(&[("RATE_LIMIT_PER_MINUTE", "3")]);
    for _ in 0..3 {
        let (s, _, _) = send(&svc, "POST", TOKEN, "203.0.113.5:4000", None, Body::empty()).await;
        assert_eq!(s, StatusCode::OK);
    }
    let (status, headers, body) =
        send(&svc, "POST", TOKEN, "203.0.113.5:4000", None, Body::empty()).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert!(
        headers["retry-after"]
            .to_str()
            .unwrap()
            .parse::<u64>()
            .unwrap()
            >= 1
    );
    assert!(!body.contains("token"), "a refusal carries no token");

    let (other, _, _) = send(
        &svc,
        "POST",
        TOKEN,
        "198.51.100.7:4000",
        None,
        Body::empty(),
    )
    .await;
    assert_eq!(other, StatusCode::OK, "someone else is unaffected");
}

#[tokio::test]
async fn behind_the_proxy_each_household_has_its_own_allowance_and_a_forged_header_buys_nothing() {
    let svc = service(&[("RATE_LIMIT_PER_MINUTE", "1"), ("TRUST_PROXY", "1")]);
    let proxy = "172.18.0.2:9000";

    let (a, _, _) = send(
        &svc,
        "POST",
        TOKEN,
        proxy,
        Some("203.0.113.1"),
        Body::empty(),
    )
    .await;
    let (b, _, _) = send(
        &svc,
        "POST",
        TOKEN,
        proxy,
        Some("203.0.113.2"),
        Body::empty(),
    )
    .await;
    assert_eq!(
        (a, b),
        (StatusCode::OK, StatusCode::OK),
        "two households behind one proxy"
    );

    // The same household again, this time prefixing forged addresses to dodge the limit.
    let (dodge, _, _) = send(
        &svc,
        "POST",
        TOKEN,
        proxy,
        Some("9.9.9.9, 203.0.113.1"),
        Body::empty(),
    )
    .await;
    assert_eq!(dodge, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn without_a_trusted_proxy_the_header_cannot_be_used_to_dodge_the_limit() {
    let svc = service(&[("RATE_LIMIT_PER_MINUTE", "1")]);
    let (first, _, _) = send(
        &svc,
        "POST",
        TOKEN,
        "203.0.113.5:4000",
        Some("1.1.1.1"),
        Body::empty(),
    )
    .await;
    let (second, _, _) = send(
        &svc,
        "POST",
        TOKEN,
        "203.0.113.5:4000",
        Some("2.2.2.2"),
        Body::empty(),
    )
    .await;
    assert_eq!(
        (first, second),
        (StatusCode::OK, StatusCode::TOO_MANY_REQUESTS)
    );
}

#[tokio::test]
async fn the_service_uses_its_injected_clock() {
    let svc = service(&[]);
    let ticks = Arc::new(AtomicU64::new(1_790_000_000));
    // Rebuild state around a clock the test controls.
    let (pem, public) = key();
    let cfg = config_with(&[], &pem).unwrap();
    let mut state = AppState::new(&cfg).unwrap();
    let t = ticks.clone();
    state.clock = Box::new(move || t.load(Ordering::Relaxed));
    let state = Arc::new(state);
    let svc2 = Service {
        app: router(state.clone()),
        state,
        public,
    };
    drop(svc);

    let (_, _, day_one) = send(&svc2, "POST", TOKEN, "203.0.113.5:1", None, Body::empty()).await;
    ticks.fetch_add(2 * 86_400, Ordering::Relaxed);
    let (_, _, day_three) = send(&svc2, "POST", TOKEN, "198.51.100.5:1", None, Body::empty()).await;

    let a: serde_json::Value = serde_json::from_str(&day_one).unwrap();
    let b: serde_json::Value = serde_json::from_str(&day_three).unwrap();
    assert_ne!(
        a["token"], b["token"],
        "two days on, a fresh token is signed"
    );
    assert!(b["expires_at"].as_u64() > a["expires_at"].as_u64());
    let _ = svc2.state.metrics.issued.load(Ordering::Relaxed);
}

// ── The Uber sign-in relay ───────────────────────────────────

/// A stand-in for Uber's token endpoint, recording every form it is sent.
async fn fake_uber() -> (String, Arc<std::sync::Mutex<Vec<HashMap<String, String>>>>) {
    let seen: Arc<std::sync::Mutex<Vec<HashMap<String, String>>>> = Arc::default();
    let record = seen.clone();
    let app = axum::Router::new().route(
        "/oauth/v2/token",
        axum::routing::post(
            move |axum::Form(form): axum::Form<HashMap<String, String>>| {
                let record = record.clone();
                async move {
                    let grant_ok = form.get("client_secret").map(String::as_str)
                        == Some("uber-secret")
                        && (form.get("code").map(String::as_str) == Some("good-code")
                            || form.get("refresh_token").map(String::as_str) == Some("good-refresh"));
                    record.lock().unwrap().push(form);
                    if grant_ok {
                        (
                            StatusCode::OK,
                            axum::Json(serde_json::json!({
                                "access_token": "access-1", "refresh_token": "refresh-2",
                                "expires_in": 2592000, "scope": "request profile",
                                "token_type": "Bearer", "last_authenticated": 1
                            })),
                        )
                    } else {
                        (
                            StatusCode::UNAUTHORIZED,
                            axum::Json(serde_json::json!({"error": "invalid_grant", "detail": "secret street"})),
                        )
                    }
                }
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/oauth/v2/token", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, seen)
}

fn uber_service(token_url: &str) -> Service {
    let (pem, public) = key();
    let mut pairs = base();
    pairs.extend_from_slice(&[
        ("UBER_CLIENT_ID", "uber-client"),
        ("UBER_CLIENT_SECRET_FILE", "/run/secrets/uber"),
        ("UBER_TOKEN_URL", token_url),
    ]);
    let cfg = Config::from_env(env(&pairs), move |path| {
        Ok(if path == "/run/secrets/uber" {
            "uber-secret\n".to_string()
        } else {
            pem.clone()
        })
    })
    .unwrap();
    let state = Arc::new(AppState::new(&cfg).unwrap());
    Service {
        app: router(state.clone()),
        state,
        public,
    }
}

async fn send_json(
    svc: &Service,
    uri: &str,
    body: serde_json::Value,
) -> (StatusCode, axum::http::HeaderMap, String) {
    let mut req = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    req.extensions_mut().insert(ConnectInfo(
        "203.0.113.9:4000".parse::<SocketAddr>().unwrap(),
    ));
    let resp = svc.app.clone().oneshot(req).await.unwrap();
    let (status, headers) = (resp.status(), resp.headers().clone());
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        headers,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

const CALLBACK: &str = "http://127.0.0.1:4000/api/v1/oauth/callback";

#[tokio::test]
async fn a_code_is_exchanged_with_the_secret_added_and_only_token_fields_come_back() {
    let (url, seen) = fake_uber().await;
    let svc = uber_service(&url);
    let (status, headers, body) = send_json(
        &svc,
        "/v1/uber/token",
        serde_json::json!({"code": "good-code", "redirect_uri": CALLBACK}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(headers["cache-control"], "no-store");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["access_token"], "access-1");
    assert_eq!(json["refresh_token"], "refresh-2");
    assert!(json.get("last_authenticated").is_none());

    let form = seen.lock().unwrap()[0].clone();
    assert_eq!(form["grant_type"], "authorization_code");
    assert_eq!(form["client_id"], "uber-client");
    assert_eq!(form["redirect_uri"], CALLBACK);
    assert!(
        !body.contains("uber-secret"),
        "the secret leaked into the reply"
    );
}

#[tokio::test]
async fn a_refresh_is_relayed() {
    let (url, seen) = fake_uber().await;
    let svc = uber_service(&url);
    let (status, _, body) = send_json(
        &svc,
        "/v1/uber/refresh",
        serde_json::json!({"refresh_token": "good-refresh"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(seen.lock().unwrap()[0]["grant_type"], "refresh_token");
    assert_eq!(svc.state.metrics.uber_relayed.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn ubers_refusal_is_passed_on_as_its_code_only() {
    let (url, _) = fake_uber().await;
    let svc = uber_service(&url);
    let (status, _, body) = send_json(
        &svc,
        "/v1/uber/token",
        serde_json::json!({"code": "used-code", "redirect_uri": CALLBACK}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("invalid_grant"), "{body}");
    assert!(!body.contains("secret street"), "{body}");
}

/// A stand-in for Uber's token endpoint that always gives the same answer.
async fn uber_answering(
    status: StatusCode,
    content_type: &'static str,
    body: &'static str,
) -> String {
    let app = axum::Router::new().route(
        "/oauth/v2/token",
        axum::routing::post(move || async move {
            (
                status,
                [(axum::http::header::CONTENT_TYPE, content_type)],
                body,
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/oauth/v2/token", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    url
}

/// Uber being down, busy or behind an error page says nothing about the member's sign-in, so it is
/// never passed on as a refusal: a pond would read that as a revoked sign-in.
#[tokio::test]
async fn uber_being_unavailable_is_not_passed_on_as_a_refusal() {
    const JSON: &str = "application/json";
    for (status, content_type, body) in [
        (
            StatusCode::SERVICE_UNAVAILABLE,
            JSON,
            r#"{"error":"temporarily_unavailable"}"#,
        ),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "text/html",
            "<html>Down for maintenance</html>",
        ),
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            JSON,
            r#"{"error":"server_error"}"#,
        ),
        (
            StatusCode::TOO_MANY_REQUESTS,
            JSON,
            r#"{"error":"too_many_requests"}"#,
        ),
        (
            StatusCode::BAD_REQUEST,
            "text/html",
            "<html>Bad request</html>",
        ),
        (
            StatusCode::OK,
            "text/html",
            "<html>Sign in to the network</html>",
        ),
    ] {
        let svc = uber_service(&uber_answering(status, content_type, body).await);
        for (route, request) in [
            (
                "/v1/uber/token",
                serde_json::json!({"code": "good-code", "redirect_uri": CALLBACK}),
            ),
            (
                "/v1/uber/refresh",
                serde_json::json!({"refresh_token": "good-refresh"}),
            ),
        ] {
            let (got, _, reply) = send_json(&svc, route, request).await;
            assert_eq!(
                got,
                StatusCode::BAD_GATEWAY,
                "{route} with Uber answering {status} {content_type}: {reply}"
            );
            assert!(!reply.contains("uber_error"), "{reply}");
        }
    }
}

#[tokio::test]
async fn a_return_address_that_is_not_a_ponds_callback_is_never_sent_to_uber() {
    let (url, seen) = fake_uber().await;
    let svc = uber_service(&url);
    let (status, _, _) = send_json(
        &svc,
        "/v1/uber/token",
        serde_json::json!({"code": "good-code", "redirect_uri": "https://evil.example/callback"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn the_client_id_is_public_and_the_relay_is_off_without_uber_settings() {
    let (url, _) = fake_uber().await;
    let svc = uber_service(&url);
    let (status, _, body) = send(
        &svc,
        "GET",
        "/v1/uber/client",
        "203.0.113.9:4000",
        None,
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("uber-client") && !body.contains("uber-secret"));

    let plain = service(&[]);
    let (status, _, _) = send(
        &plain,
        "GET",
        "/v1/uber/client",
        "203.0.113.9:4000",
        None,
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let (status, _, _) = send_json(
        &plain,
        "/v1/uber/refresh",
        serde_json::json!({"refresh_token": "good-refresh"}),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}

/// One allowance across MusicKit and Uber: a pond cannot dodge the limit by switching routes.
#[tokio::test]
async fn uber_requests_share_the_rate_limit() {
    let (url, _) = fake_uber().await;
    let (pem, public) = key();
    let mut pairs = base();
    pairs.extend_from_slice(&[
        ("RATE_LIMIT_PER_MINUTE", "1"),
        ("UBER_CLIENT_ID", "uber-client"),
        ("UBER_CLIENT_SECRET_FILE", "/run/secrets/uber"),
        ("UBER_TOKEN_URL", url.as_str()),
    ]);
    let cfg = Config::from_env(env(&pairs), move |_| Ok(pem.clone())).unwrap();
    let state = Arc::new(AppState::new(&cfg).unwrap());
    let svc = Service {
        app: router(state.clone()),
        state,
        public,
    };

    send(&svc, "POST", TOKEN, "203.0.113.9:4000", None, Body::empty()).await;
    let (status, _, _) = send_json(
        &svc,
        "/v1/uber/refresh",
        serde_json::json!({"refresh_token": "good-refresh"}),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
}
