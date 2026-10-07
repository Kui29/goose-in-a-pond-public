//! A pond whose phone routes book rides with a stand-in company, and member phones paired to it.
//! Ride booking is process-wide, so each test binary installs its own company before `pond()`.
// Each test binary that includes this uses only part of it.
#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::{Arc, LazyLock};

use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{Method, Request, StatusCode};
use pond_api::build_router;
use pond_core::user_data::domain::profile::CreateProfileRequest;
use serde_json::{json, Value};
use tower::ServiceExt;

static CREDENTIAL: LazyLock<pond_api::host_guard::HostCredential> =
    LazyLock::new(pond_api::host_guard::HostCredential::generate);

pub struct Pond {
    pub loopback: axum::Router,
    pub lan: axum::Router,
    pub profiles: Arc<dyn pond_core::user_data::ports::profile::ProfileRepository + Send + Sync>,
    pub settings: Arc<dyn pond_core::user_data::ports::settings::SettingsRepository + Send + Sync>,
    _dir: tempfile::TempDir,
}

/// A pond with travel switched on, as booking needs.
pub async fn pond() -> Pond {
    let pond_api::test_support::TestState { state, dir, .. } =
        pond_api::test_support::app_state().await;
    let profiles = state.profile_repo.clone();
    let settings = state.settings_repo.clone();
    settings
        .set_key("ext_travel_enabled", "true".to_string())
        .await
        .unwrap();
    let state = Arc::new(state);
    let dist = std::path::PathBuf::from("pond-desktop/dist");
    let router = |ip: [u8; 4]| {
        build_router(state.clone(), dist.clone())
            .layer(axum::Extension(CREDENTIAL.clone()))
            .layer(MockConnectInfo(SocketAddr::from((ip, 40_000))))
    };
    Pond {
        loopback: router([127, 0, 0, 1]),
        lan: router([192, 168, 1, 44]),
        profiles,
        settings,
        _dir: dir,
    }
}

pub async fn send(
    router: &axum::Router,
    method: Method,
    uri: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header(pond_api::host_guard::CREDENTIAL_HEADER, CREDENTIAL.as_str());
    if let Some(token) = token {
        req = req.header("Authorization", format!("Bearer {token}"));
    }
    let req = match body {
        Some(b) => req
            .header("Content-Type", "application/json")
            .body(Body::from(b.to_string()))
            .unwrap(),
        None => req.body(Body::empty()).unwrap(),
    };
    let resp = router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn client_mac(code: &str, challenge_b64: &str, client_id: &str) -> String {
    use base64::Engine as _;
    use hmac::Mac as _;
    let challenge = base64::engine::general_purpose::STANDARD
        .decode(challenge_b64.as_bytes())
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

/// A member, and a phone paired to them through a pairing code that names them.
pub async fn member_with_phone(p: &Pond, name: &str) -> String {
    member_and_phone(p, name).await.1
}

/// As [`member_with_phone`], with the member's profile id first.
pub async fn member_and_phone(p: &Pond, name: &str) -> (String, String) {
    let profile_id = p
        .profiles
        .create(CreateProfileRequest {
            display_name: name.to_string(),
            avatar_emoji: "*".to_string(),
        })
        .await
        .unwrap()
        .id;
    let phone = paired_phone(
        p,
        &format!("{name}-phone"),
        json!({"profile_id": profile_id}),
    )
    .await;
    (profile_id, phone)
}

/// A phone paired through a code that names nobody: it belongs to no member.
pub async fn phone_of_nobody(p: &Pond, name: &str) -> String {
    paired_phone(p, &format!("{name}-phone"), json!({"unattributed": true})).await
}

async fn paired_phone(p: &Pond, client_id: &str, code_request: Value) -> String {
    let (status, issued) = send(
        &p.loopback,
        Method::POST,
        "/api/v1/handshake/pairing-code",
        None,
        Some(code_request),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{issued}");
    let code = issued["code"].as_str().unwrap().to_string();
    let (_, init) = send(
        &p.loopback,
        Method::POST,
        "/api/v1/handshake/init",
        None,
        Some(json!({"client_id": client_id, "client_type": "gotg", "client_version": "1.0.0"})),
    )
    .await;
    let (_, verified) = send(
        &p.loopback,
        Method::POST,
        "/api/v1/handshake/verify",
        None,
        Some(json!({
            "challenge_id": init["challenge_id"],
            "mac": client_mac(&code, init["challenge"].as_str().unwrap(), client_id),
            "device_name": client_id,
        })),
    )
    .await;
    verified["session_token"].as_str().unwrap().to_string()
}

/// A trip across Nairobi, home to the airport.
pub fn trip() -> Value {
    json!({
        "pickup": {"latitude": -1.2676, "longitude": 36.8108},
        "dropoff": {"name": "JKIA", "latitude": -1.319167, "longitude": 36.9275},
    })
}
