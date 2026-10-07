//! Transport boundary tests use real SQLite challenges and never trust forwarding headers.
use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{Method, Request, StatusCode};
use pond_api::{build_router, AppState};

use pond_infra::sqlite_event_log::SqliteEventLog;
use pond_infra::sqlite_handshake::SqliteHandshakeAdapter;
use serde_json::Value;
use tower::ServiceExt;

struct Harness {
    loopback: axum::Router,
    remote: axum::Router,
    handshake: Arc<SqliteHandshakeAdapter>,
    companion: axum::Router,
    state: Arc<AppState>,
    event_log: Arc<SqliteEventLog>,
    _tmp: tempfile::TempDir,
}

async fn make_app() -> Harness {
    let pond_api::test_support::TestState {
        mut state,
        handshake,
        dir,
    } = pond_api::test_support::app_state().await;
    // A real event log, so a test can read back the audit event a pairing records.
    let event_log = Arc::new(SqliteEventLog::new(state.db.logs.clone()));
    state.event_log = Some(event_log.clone());
    let state = Arc::new(state);

    let dist = std::path::PathBuf::from("pond-desktop/dist");
    let loopback = build_router(state.clone(), dist.clone())
        .layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 40_000))));
    let companion = pond_api::build_companion_router(state.clone());
    let remote = build_router(state.clone(), dist).layer(MockConnectInfo(SocketAddr::from((
        [100, 64, 0, 44],
        40_000,
    ))));

    Harness {
        loopback,
        remote,
        handshake,
        companion,
        state,
        event_log,
        _tmp: dir,
    }
}

use pond_core::security::ports::handshake::{Handshake, HandshakeRequest};
use serde_json::json;

async fn post(router: &axum::Router, path: &str, body: Value) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(path)
                .header("Content-Type", "application/json")
                .header("X-Forwarded-For", "127.0.0.1")
                .header("Forwarded", "for=192.168.1.2")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 65536)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn tailnet_cannot_pair_even_with_forged_local_headers() {
    let h = make_app().await;
    for (path, body) in [
        (
            "/api/v1/handshake",
            json!({"client_id":"remote", "client_type":"gotg", "client_version":"test", "pairing_code":"123456"}),
        ),
        (
            "/api/v1/handshake/init",
            json!({"client_id":"remote", "client_type":"gotg", "client_version":"test"}),
        ),
        (
            "/api/v1/handshake/verify",
            json!({"challenge_id":"none", "mac":"00"}),
        ),
    ] {
        let (status, body) = post(&h.remote, path, body).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["error"], "pairing_requires_lan");
    }
}

#[tokio::test]
async fn a_locally_started_challenge_cannot_be_completed_remotely() {
    let h = make_app().await;
    let (status, challenge) = post(
        &h.loopback,
        "/api/v1/handshake/init",
        json!({
            "client_id":"phone", "client_type":"gotg", "client_version":"test"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = post(
        &h.remote,
        "/api/v1/handshake/verify",
        json!({
            "challenge_id": challenge["challenge_id"], "mac":"00"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "pairing_requires_lan");
}

#[tokio::test]
async fn an_existing_session_can_refresh_from_the_tailnet() {
    let h = make_app().await;
    let code = h.handshake.issue_pairing_code().await.unwrap();
    let paired = h
        .handshake
        .handshake(HandshakeRequest {
            client_id: "phone".into(),
            client_type: "gotg".into(),
            client_version: "test".into(),
            pairing_code: Some(code.code),
        })
        .await
        .unwrap();
    assert!(paired.accepted);
    let (status, refreshed) = post(
        &h.remote,
        "/api/v1/handshake/refresh",
        json!({
            "refresh_token": paired.refresh_token.unwrap()
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(refreshed["accepted"], true);
}

/// The dashboard paths the companion listener must never serve.
const DASHBOARD_PATHS: [&str; 4] = ["/", "/dev/test", "/dev/face", "/assets/index.js"];

async fn get(router: &axum::Router, path: &str, bearer: Option<&str>) -> StatusCode {
    let mut request = Request::builder().uri(path);
    if let Some(token) = bearer {
        request = request.header("Authorization", format!("Bearer {token}"));
    }
    router
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap()
        .status()
}

/// Pair a device the honest way and return its session token.
async fn session_token(h: &Harness) -> String {
    let code = h.handshake.issue_pairing_code().await.unwrap();
    let paired = h
        .handshake
        .handshake(HandshakeRequest {
            client_id: "phone".into(),
            client_type: "gotg".into(),
            client_version: "test".into(),
            pairing_code: Some(code.code),
        })
        .await
        .unwrap();
    assert!(paired.accepted);
    paired.session_token.unwrap()
}

#[tokio::test]
async fn companion_never_serves_dashboard_and_missing_peer_fails_closed() {
    let h = make_app().await;

    // A real token still gets 404: the routes are absent, not merely shadowed by middleware.
    let token = session_token(&h).await;
    for path in DASHBOARD_PATHS {
        assert_eq!(
            get(&h.companion, path, Some(&token)).await,
            StatusCode::NOT_FOUND,
            "{path}"
        );
    }

    // Anonymous: non-API paths are `Exposure::HostOnly` and a missing peer isn't loopback, so
    // the token check answers 401 first; every path gets it, so it can't probe for a route.
    for path in DASHBOARD_PATHS {
        assert_eq!(
            get(&h.companion, path, None).await,
            StatusCode::UNAUTHORIZED,
            "{path}"
        );
    }
    let (status, body) = post(
        &h.companion,
        "/api/v1/handshake/init",
        json!({
            "client_id":"phone", "client_type":"gotg", "client_version":"test"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "pairing_requires_lan");
}

/// This host's own address on an attached LAN: the one non-loopback peer the LAN check
/// admits in-process. `None` on a host with no LAN, where the test says it was skipped.
fn own_lan_address() -> Option<std::net::IpAddr> {
    pond_api::network::interfaces()
        .ok()?
        .into_iter()
        .filter(pond_api::network::is_lan_interface)
        .map(|interface| interface.ip())
        .find(|ip| {
            ip.is_ipv4()
                && pond_api::network::is_lan_peer(
                    *ip,
                    &pond_api::network::interfaces().unwrap_or_default(),
                )
        })
}

#[tokio::test]
async fn a_phone_on_the_lan_must_bind_the_key_it_pinned() {
    let Some(address) = own_lan_address() else {
        eprintln!("SKIPPED: this host has no attached LAN, so no LAN peer can be simulated");
        return;
    };
    let h = make_app().await;
    let state_router = h.companion.clone();
    let lan = state_router.layer(MockConnectInfo(SocketAddr::new(address, 40_000)));
    let code = h.handshake.issue_pairing_code().await.unwrap().code;
    let (status, challenge) = post(
        &lan,
        "/api/v1/handshake/init",
        json!({"client_id":"lan-phone", "client_type":"gotg", "client_version":"test"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    use base64::Engine;
    use hmac::Mac;
    let raw = base64::engine::general_purpose::STANDARD
        .decode(challenge["challenge"].as_str().unwrap())
        .unwrap();
    let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(code.as_bytes()).unwrap();
    mac.update(&raw);
    mac.update(b"lan-phone");
    let unbound: String = mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let verify = json!({"challenge_id": challenge["challenge_id"], "mac": unbound});

    let (status, body) = post(&lan, "/api/v1/handshake/verify", verify.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "channel_binding_required");

    // The refusal spent nothing: the same challenge still completes from loopback.
    let (status, body) = post(&h.loopback, "/api/v1/handshake/verify", verify).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["accepted"], true);

    let (status, body) = post(
        &lan,
        "/api/v1/handshake",
        json!({"client_id":"lan-phone", "client_type":"gotg", "client_version":"test", "pairing_code": code}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "legacy_pairing_host_only");
}

/// A phone on the companion router as a loopback peer, so the LAN guard passes on any host.
fn on_loopback(router: axum::Router) -> axum::Router {
    router.layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 40_001))))
}

/// The original unbound transcript, `challenge || client_id`, keyed by the pairing code: what a
/// client with no TLS key to bind (Expo Go over the plaintext listener) sends.
fn unbound_mac(code: &str, challenge_b64: &str, client_id: &str) -> String {
    use base64::Engine as _;
    use hmac::Mac as _;
    let challenge = base64::engine::general_purpose::STANDARD
        .decode(challenge_b64)
        .unwrap();
    let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(code.as_bytes()).unwrap();
    mac.update(&challenge);
    mac.update(client_id.as_bytes());
    mac.finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Pair unbound through `router` and return the `transport` attribute of the audit event.
async fn pair_unbound(h: &Harness, router: &axum::Router, client_id: &str) -> Option<String> {
    let code = h.handshake.issue_pairing_code().await.unwrap().code;
    let (status, challenge) = post(
        router,
        "/api/v1/handshake/init",
        json!({"client_id": client_id, "client_type": "gotg", "client_version": "test"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{challenge}");
    let mac = unbound_mac(&code, challenge["challenge"].as_str().unwrap(), client_id);
    let (status, body) = post(
        router,
        "/api/v1/handshake/verify",
        json!({
            "challenge_id": challenge["challenge_id"], "mac": mac,
            "device_name": client_id, "channel_binding": null
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["accepted"], true, "{body}");
    assert!(
        body["server_proof"].is_null(),
        "an unbound pair has nothing to prove over"
    );

    use pond_core::security::domain::event::{AttributeValue, EventCategory, EventQuery};
    use pond_core::security::ports::event_log::EventLog;
    let events = h
        .event_log
        .query(EventQuery {
            category: Some(EventCategory::Auth),
            ..Default::default()
        })
        .await
        .unwrap();
    let event = events
        .iter()
        .find(|e| {
            e.action == "auth.device_paired"
                && e.attributes.get("device_name") == Some(&AttributeValue::Text(client_id.into()))
        })
        .expect("the pairing is audited");
    match event.attributes.get("transport") {
        Some(AttributeValue::Text(transport)) => Some(transport.clone()),
        None => None,
        Some(other) => panic!("unexpected transport attribute {other:?}"),
    }
}

/// The plaintext development listener pairs unbound, because there is no TLS key to bind, and
/// the audit trail records that it did. The HTTPS listener's pairing is unchanged by the flag.
#[tokio::test]
async fn unbound_pairing_on_the_insecure_listener_is_audited_as_plaintext() {
    use pond_api::insecure_dev::{advertise, router, InsecureDevLan};
    let h = make_app().await;
    let lan = Some(InsecureDevLan { port: 4080 });
    let https = on_loopback(advertise(h.companion.clone(), lan));
    let insecure = on_loopback(router(advertise(h.companion.clone(), lan)));

    assert_eq!(
        pair_unbound(&h, &insecure, "expo-go").await.as_deref(),
        Some("insecure_dev")
    );
    assert_eq!(pair_unbound(&h, &https, "release-phone").await, None);
}

async fn system_info(router: &axum::Router, bearer: Option<&str>) -> Value {
    let mut request = Request::builder().uri("/api/v1/system/info");
    if let Some(token) = bearer {
        request = request.header("Authorization", format!("Bearer {token}"));
    }
    let response = router
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 65536)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn system_info_names_the_insecure_listener_only_while_it_runs() {
    use pond_api::insecure_dev::{advertise, router, InsecureDevLan};
    let h = make_app().await;
    let token = session_token(&h).await;
    let lan = Some(InsecureDevLan { port: 4080 });
    let off = on_loopback(advertise(
        pond_api::build_companion_router(h.state.clone()),
        None,
    ));
    let https = on_loopback(advertise(h.companion.clone(), lan));
    let insecure = on_loopback(router(advertise(h.companion.clone(), lan)));

    for bearer in [None, Some(token.as_str())] {
        let info = system_info(&off, bearer).await;
        assert!(info.get("insecure_dev").is_none(), "{info}");
        for on in [&https, &insecure] {
            assert_eq!(system_info(on, bearer).await["insecure_dev"], true);
        }
    }
}

#[tokio::test]
async fn a_loopback_peer_on_the_plaintext_listener_is_not_the_host() {
    use pond_api::insecure_dev::{advertise, router, InsecureDevLan};
    let h = make_app().await;
    let lan = Some(InsecureDevLan { port: 4080 });
    // The middleware's refusal, as opposed to the handler's own internal-token check.
    let refused_by_middleware = |app: axum::Router| async move {
        let response = app
            .oneshot(
                Request::get("/api/v1/player/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        body["error"] == "Missing Authorization header"
    };
    // A host-only route: the host process gets past the middleware without a token...
    assert!(!refused_by_middleware(from_this_machine(h.companion.clone())).await);
    // ...but whatever reaches the plaintext door from this machine is not the host.
    let insecure = from_this_machine(router(advertise(h.companion.clone(), lan)));
    assert!(refused_by_middleware(insecure).await);
}

/// A loopback peer as the server records it. `MockConnectInfo` feeds only the extractor, and the
/// auth middleware reads the extension itself, so under the mock no caller is ever the host.
fn from_this_machine(router: axum::Router) -> axum::Router {
    router.layer(axum::Extension(axum::extract::ConnectInfo(
        SocketAddr::from(([127, 0, 0, 1], 40_002)),
    )))
}
