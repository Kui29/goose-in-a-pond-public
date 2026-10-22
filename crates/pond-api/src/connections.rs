//! The household's connections and pairing settings (`docs/architecture/pond-connections.md`).
//! Reads take any paired token; the writes can take the pond off the network, so each asks first.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::{Extension, Json};
use chrono::SecondsFormat;
use pond_core::network::domain::setup_record::ClaimState;
use pond_core::network::domain::wifi::{
    is_valid_ssid, WifiForgetError, WifiJoinError, WifiPassphrase, WifiScanError,
};
use pond_core::network::ports::setup_radios::SetupRadios;
use pond_core::network::ports::wired_interfaces::WiredInterfaces;
use pond_core::network::services::connection_changes::NOT_ALLOWED;
use pond_core::network::services::wifi_connections::WifiConnections;
use pond_core::security::domain::proven_device::ProvenDevice;
use pond_core::security::ports::policy::Principal;
use pond_core::user_data::services::household_access::{may, Action, Refusal as AccessRefusal};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::host_guard::HostCredential;
use crate::network::{availability, NO_SETUP_CHANNEL};
use crate::AppState;

/// A join body is an SSID, a passphrase and a flag; nothing larger is a join.
pub const JOIN_BODY_LIMIT: usize = 4096;

const UNSUPPORTED: &str = "unsupported";
const INVALID_REQUEST: &str = "invalid_request";

/// What the connection routes drive, wired by the process that owns the station. Its station must
/// be the one the setup channel holds, so a join over Bluetooth and one over HTTPS never overlap.
pub struct Connections {
    wifi: WifiConnections,
    wired: Arc<dyn WiredInterfaces>,
    hostname: String,
    https_port: u16,
}

impl Connections {
    pub fn new(
        wifi: WifiConnections,
        wired: Arc<dyn WiredInterfaces>,
        hostname: impl Into<String>,
        https_port: u16,
    ) -> Self {
        Self {
            wifi,
            wired,
            hostname: hostname.into(),
            https_port,
        }
    }
}

type Refusal = (StatusCode, Json<Value>);

fn refuse(status: StatusCode, code: &str) -> Refusal {
    (status, Json(json!({ "error": code })))
}

/// A server wired without a station has nothing to show or change.
fn wired(connections: Option<Extension<Arc<Connections>>>) -> Result<Arc<Connections>, Refusal> {
    connections
        .map(|Extension(connections)| connections)
        .ok_or_else(|| refuse(StatusCode::SERVICE_UNAVAILABLE, UNSUPPORTED))
}

/// Whether this caller may use the write routes.
async fn change_access(
    state: &AppState,
    principal: Option<&Principal>,
    credential: Option<&HostCredential>,
    headers: &HeaderMap,
) -> Result<(), AccessRefusal> {
    let caller = crate::household_access::caller(state, principal, credential, headers).await;
    may(&caller, Action::ChangeConnections)
}

/// The write routes' gate: `403 not_allowed`, logged with who asked and why not.
async fn require_changer(
    state: &AppState,
    principal: Option<&Principal>,
    credential: Option<&HostCredential>,
    headers: &HeaderMap,
    operation: &'static str,
) -> Result<(), Refusal> {
    change_access(state, principal, credential, headers)
        .await
        .map_err(|refusal| {
            tracing::warn!(
                kind = "connections_change_denied",
                operation,
                role = ?refusal.role,
                caller = principal
                    .and_then(|p| p.device_id.as_deref())
                    .unwrap_or("none"),
                "refused to change the pond's network for this caller"
            );
            refuse(StatusCode::FORBIDDEN, NOT_ALLOWED)
        })
}

async fn ble_enabled(state: &AppState) -> Option<bool> {
    match state.settings_repo.get().await {
        Ok(settings) => Some(settings.ble_setup_enabled),
        Err(error) => {
            tracing::warn!(%error, "connections: settings unreadable");
            None
        }
    }
}

fn bluetooth(enabled: Option<bool>, radios: Option<&Arc<dyn SetupRadios>>) -> Value {
    let Some(radios) = radios else {
        return json!({"enabled": enabled, "state": "unavailable", "reason": NO_SETUP_CHANNEL,
                      "advertising_as": null, "session_open": false});
    };
    let ble = radios.ble();
    json!({
        "enabled": enabled,
        "state": ble.as_str(),
        "reason": ble.reason(),
        "advertising_as": radios.advertising_as(),
        "session_open": radios.session_open(),
    })
}

/// `GET /api/v1/connections`: Wi-Fi, Ethernet and Bluetooth at once, and whether this caller may
/// change them.
pub(crate) async fn connections(
    State(state): State<Arc<AppState>>,
    connections: Option<Extension<Arc<Connections>>>,
    radios: Option<Extension<Arc<dyn SetupRadios>>>,
    principal: Option<Extension<Principal>>,
    credential: Option<Extension<HostCredential>>,
    headers: HeaderMap,
) -> Result<Json<Value>, Refusal> {
    let connections = wired(connections)?;
    let principal = principal.as_ref().map(|Extension(p)| p);
    let credential = credential.as_ref().map(|Extension(c)| c);
    let ((link, capabilities), ethernet, access, enabled) = tokio::join!(
        connections.wifi.link(),
        connections.wired.wired_interfaces(),
        change_access(&state, principal, credential, &headers),
        ble_enabled(&state),
    );
    let radios = radios.as_ref().map(|Extension(radios)| radios);
    Ok(Json(json!({
        "hostname": connections.hostname,
        "https_port": connections.https_port,
        "wifi": {
            "state": link.state,
            "reason": link.reason,
            "ssid": link.ssid,
            "address": link.address,
            "signal": link.signal,
            "capabilities": {
                "scan": availability(&capabilities.scan),
                "join": availability(&capabilities.join),
                "forget": availability(&capabilities.forget),
            },
        },
        "ethernet": ethernet,
        "bluetooth": bluetooth(enabled, radios),
        "can_change": {
            "allowed": access.is_ok(),
            "reason": access.err().map(|refusal| refusal.to_string()),
        },
    })))
}

/// `GET /api/v1/connections/wifi/networks`: a fresh scan, ranked.
pub(crate) async fn wifi_networks(
    connections: Option<Extension<Arc<Connections>>>,
) -> Result<Json<Value>, Refusal> {
    let connections = wired(connections)?;
    match connections.wifi.networks().await {
        Ok(networks) => Ok(Json(json!({ "networks": networks }))),
        Err(error) => Err(refuse(scan_status(error), error.code())),
    }
}

/// The passphrase never reaches `Debug`, `Serialize` or a log (`WifiPassphrase`).
#[derive(Deserialize)]
struct JoinBody {
    ssid: String,
    #[serde(default)]
    passphrase: Option<WifiPassphrase>,
    #[serde(default)]
    hidden: bool,
}

/// `POST /api/v1/connections/wifi/join`: waits for the join, up to the contract's 45 s.
pub(crate) async fn wifi_join(
    State(state): State<Arc<AppState>>,
    connections: Option<Extension<Arc<Connections>>>,
    principal: Option<Extension<Principal>>,
    credential: Option<Extension<HostCredential>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, Refusal> {
    let principal = principal.as_ref().map(|Extension(p)| p);
    let credential = credential.as_ref().map(|Extension(c)| c);
    require_changer(&state, principal, credential, &headers, "wifi_join").await?;
    let connections = wired(connections)?;
    // The parse error is dropped unread: for a mistyped passphrase it would quote the value.
    let request = match serde_json::from_slice::<JoinBody>(&body) {
        Ok(request) if is_valid_ssid(&request.ssid) => request,
        _ => return Err(refuse(StatusCode::BAD_REQUEST, INVALID_REQUEST)),
    };
    let JoinBody {
        ssid,
        passphrase,
        hidden,
    } = request;
    tracing::info!(
        %ssid,
        hidden,
        with_passphrase = passphrase.is_some(),
        caller = principal.and_then(|p| p.device_id.as_deref()).unwrap_or("host"),
        "joining Wi-Fi for the household's connections settings"
    );
    match connections.wifi.join(ssid, passphrase, hidden).await {
        Ok(joined) => Ok(Json(json!({
            "ssid": joined.ssid,
            "address": joined.ipv4.to_string(),
            "https_port": connections.https_port,
            "hostname": connections.hostname,
        }))),
        Err(error) => Err(refuse(join_status(error), error.code())),
    }
}

#[derive(Deserialize)]
pub(crate) struct ForgetQuery {
    #[serde(default)]
    confirm: bool,
}

/// `DELETE /api/v1/connections/wifi/networks/{ssid}[?confirm=true]`.
pub(crate) async fn wifi_forget(
    State(state): State<Arc<AppState>>,
    connections: Option<Extension<Arc<Connections>>>,
    principal: Option<Extension<Principal>>,
    credential: Option<Extension<HostCredential>>,
    headers: HeaderMap,
    ssid: Result<Path<String>, PathRejection>,
    query: Result<Query<ForgetQuery>, QueryRejection>,
) -> Result<StatusCode, Refusal> {
    let principal = principal.as_ref().map(|Extension(p)| p);
    let credential = credential.as_ref().map(|Extension(c)| c);
    require_changer(&state, principal, credential, &headers, "wifi_forget").await?;
    let connections = wired(connections)?;
    let (Ok(Path(ssid)), Ok(Query(query))) = (ssid, query) else {
        return Err(refuse(StatusCode::BAD_REQUEST, INVALID_REQUEST));
    };
    if !is_valid_ssid(&ssid) {
        return Err(refuse(StatusCode::BAD_REQUEST, INVALID_REQUEST));
    }
    match connections.wifi.forget(&ssid, query.confirm).await {
        Ok(()) => {
            tracing::info!(
                %ssid,
                confirmed = query.confirm,
                caller = principal.and_then(|p| p.device_id.as_deref()).unwrap_or("host"),
                "forgot a saved Wi-Fi network for the household's connections settings"
            );
            Ok(StatusCode::NO_CONTENT)
        }
        Err(error) => Err(refuse(forget_status(error), error.code())),
    }
}

/// `GET /api/v1/setup/status`: the setup card's state for this caller, never its code.
pub(crate) async fn setup_status(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
) -> Result<Json<Value>, Refusal> {
    let device = principal
        .as_ref()
        .map_or_else(ProvenDevice::none, |Extension(p)| {
            ProvenDevice::from_principal(p)
        });
    let status = match &state.setup_claims {
        Some(claims) => claims.setup_status().await.map_err(|error| {
            tracing::warn!(%error, "setup status: the setup record is unreadable");
            refuse(StatusCode::INTERNAL_SERVER_ERROR, "internal_error")
        })?,
        None => None,
    };
    let Some(status) = status else {
        return Ok(Json(
            json!({"id": null, "claimed": false, "claimed_at": null,
                              "claimed_by_this_device": false}),
        ));
    };
    let claimed_at = match status.claim {
        ClaimState::Claimed { at } => Some(at.to_rfc3339_opts(SecondsFormat::Secs, true)),
        ClaimState::Unclaimed => None,
    };
    Ok(Json(json!({
        "id": status.discriminator.to_string(),
        "claimed": status.claim.is_claimed(),
        "claimed_at": claimed_at,
        "claimed_by_this_device": status.claimed_by(device.id()),
    })))
}

fn scan_status(error: WifiScanError) -> StatusCode {
    match error {
        WifiScanError::NotPermitted => StatusCode::FORBIDDEN,
        WifiScanError::Unsupported => StatusCode::SERVICE_UNAVAILABLE,
        WifiScanError::Busy => StatusCode::CONFLICT,
        WifiScanError::Timeout => StatusCode::GATEWAY_TIMEOUT,
    }
}

fn join_status(error: WifiJoinError) -> StatusCode {
    match error {
        WifiJoinError::WrongPassword => StatusCode::UNAUTHORIZED,
        WifiJoinError::NotFound => StatusCode::NOT_FOUND,
        WifiJoinError::NoAddress => StatusCode::BAD_GATEWAY,
        WifiJoinError::Timeout => StatusCode::GATEWAY_TIMEOUT,
        WifiJoinError::NotPermitted => StatusCode::FORBIDDEN,
        WifiJoinError::Unsupported => StatusCode::SERVICE_UNAVAILABLE,
        WifiJoinError::Busy => StatusCode::CONFLICT,
        WifiJoinError::UnsupportedSecurity => StatusCode::BAD_REQUEST,
    }
}

fn forget_status(error: WifiForgetError) -> StatusCode {
    match error {
        WifiForgetError::NotFound => StatusCode::NOT_FOUND,
        WifiForgetError::InUse => StatusCode::CONFLICT,
        WifiForgetError::NotPermitted => StatusCode::FORBIDDEN,
        WifiForgetError::Unsupported => StatusCode::SERVICE_UNAVAILABLE,
        WifiForgetError::Busy => StatusCode::CONFLICT,
    }
}
