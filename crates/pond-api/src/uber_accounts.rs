//! Connecting a household member's own Uber account, from the pond's desktop. Sign-in goes
//! through Jarida's credentials service, which holds the Uber app's client secret; the member's
//! tokens are kept in this pond's secret store (`pond_adapters_uber::accounts`).

use std::sync::Arc;

use axum::extract::{ConnectInfo, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::{Extension, Json};
use pond_adapters_uber::accounts::{authorize_url, SignInRelay, UberAccounts};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::host_guard::HostCredential;
use crate::oauth_callback::{self, FlowOutcome, PkceSession};
use crate::AppState;

/// `PkceSession::provider_id` for an Uber sign-in, so the shared callback hands it here.
pub const PROVIDER_ID: &str = "uber";

type Refusal = (StatusCode, Json<Value>);

fn refuse(status: StatusCode, message: &str) -> Refusal {
    (status, Json(json!({ "error": message })))
}

/// Connecting someone's Uber account is for the pond's own desktop, never a paired phone.
fn host_only(
    peer: std::net::SocketAddr,
    credential: Option<&HostCredential>,
    headers: &HeaderMap,
    operation: &'static str,
) -> Result<(), Refusal> {
    if !peer.ip().is_loopback() {
        return Err(refuse(
            StatusCode::FORBIDDEN,
            "Uber accounts are managed on the pond itself",
        ));
    }
    crate::host_guard::require_credential(credential, headers, operation)
}

fn accounts(state: &AppState) -> Result<UberAccounts, Refusal> {
    let secrets = state.secret_repo.clone().ok_or_else(|| {
        refuse(
            StatusCode::SERVICE_UNAVAILABLE,
            "this pond has no secret store to keep Uber sign-ins in",
        )
    })?;
    let relay_url = crate::musickit::managed_url().ok_or_else(|| {
        refuse(
            StatusCode::SERVICE_UNAVAILABLE,
            "Uber sign-in needs Jarida's credentials service, which is turned off on this pond \
             (POND_CREDENTIALS_URL)",
        )
    })?;
    Ok(UberAccounts::new(
        secrets,
        SignInRelay::new(state.http_client.clone(), &relay_url),
    ))
}

fn callback_uri(api_port: u16) -> String {
    format!("http://127.0.0.1:{api_port}/api/v1/oauth/callback")
}

#[derive(Deserialize)]
pub struct ConnectRequest {
    pub profile_id: String,
}

/// `POST /api/v1/uber/accounts/connect` — start a member's sign-in; the desktop opens `auth_url`
/// and follows the outcome at `/oauth/status/{state}`.
pub async fn connect(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    credential: Option<Extension<HostCredential>>,
    headers: HeaderMap,
    Json(request): Json<ConnectRequest>,
) -> Result<Json<Value>, Refusal> {
    host_only(
        peer,
        credential.as_ref().map(|Extension(c)| c),
        &headers,
        "uber_connect",
    )?;
    let profile_id = request.profile_id.trim().to_string();
    match state.profile_repo.get(&profile_id).await {
        Ok(Some(_)) => {}
        Ok(None) => return Err(refuse(StatusCode::NOT_FOUND, "no such household member")),
        Err(e) => {
            tracing::warn!(error = %e, "uber connect: could not read the member");
            return Err(refuse(
                StatusCode::SERVICE_UNAVAILABLE,
                "could not read the household member",
            ));
        }
    }

    let accounts = accounts(&state)?;
    let client_id = accounts.relay().client_id().await.map_err(|e| {
        tracing::warn!(error = %e, "uber connect: no client id from the credentials service");
        refuse(
            StatusCode::BAD_GATEWAY,
            "Jarida's credentials service could not start an Uber sign-in",
        )
    })?;

    let nonce = oauth_callback::generate_state();
    state.oauth_state.write().await.insert(
        nonce.clone(),
        PkceSession {
            provider_id: PROVIDER_ID.to_string(),
            // Uber documents no PKCE; the code is bound to this attempt by `state` alone.
            code_verifier: String::new(),
            extension_id: None,
            profile_id: Some(profile_id),
            created_at: std::time::Instant::now(),
        },
    );
    Ok(Json(json!({
        "auth_url": authorize_url(&client_id, &callback_uri(state.api_port), &nonce),
        "state": nonce,
    })))
}

/// `GET /api/v1/uber/accounts` — which members have connected Uber.
pub async fn list(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    credential: Option<Extension<HostCredential>>,
    headers: HeaderMap,
) -> Result<Json<Value>, Refusal> {
    host_only(
        peer,
        credential.as_ref().map(|Extension(c)| c),
        &headers,
        "uber_accounts",
    )?;
    let connected = accounts(&state)?.connected_members().await.map_err(|e| {
        tracing::warn!(error = %e, "uber accounts: could not read the secret store");
        refuse(
            StatusCode::SERVICE_UNAVAILABLE,
            "could not read Uber connections",
        )
    })?;
    Ok(Json(json!({ "connected": connected })))
}

/// `DELETE /api/v1/uber/accounts/{profile_id}` — forget a member's Uber connection on this pond.
pub async fn disconnect(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    credential: Option<Extension<HostCredential>>,
    headers: HeaderMap,
    Path(profile_id): Path<String>,
) -> Result<StatusCode, Refusal> {
    host_only(
        peer,
        credential.as_ref().map(|Extension(c)| c),
        &headers,
        "uber_disconnect",
    )?;
    accounts(&state)?
        .disconnect(&profile_id)
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "uber disconnect: could not update the secret store");
            refuse(
                StatusCode::SERVICE_UNAVAILABLE,
                "could not forget the Uber connection",
            )
        })?;
    Ok(StatusCode::NO_CONTENT)
}

/// Uber's return to the shared OAuth callback: exchange the code through the relay and keep the
/// member's tokens. Records the outcome for the desktop's status poll either way.
pub async fn finish_sign_in(
    state: &AppState,
    session: PkceSession,
    code: &str,
    nonce: &str,
) -> Response {
    let outcome = match (session.profile_id.as_deref(), code.trim()) {
        (None, _) => Err("this sign-in was not started for a household member".to_string()),
        (_, "") => Err("Uber did not return a sign-in code; the sign-in was cancelled".to_string()),
        (Some(profile_id), code) => match accounts(state) {
            Err((_, Json(body))) => {
                Err(body["error"].as_str().unwrap_or("unavailable").to_string())
            }
            Ok(accounts) => accounts
                .connect(
                    profile_id,
                    code,
                    &callback_uri(state.api_port),
                    chrono::Utc::now(),
                )
                .await
                .map_err(|e| format!("{e:#}")),
        },
    };

    match outcome {
        Ok(()) => {
            oauth_callback::record_outcome(&state.oauth_outcomes, nonce, FlowOutcome::Completed)
                .await;
            Html(
                "<h1>Uber is connected</h1><p>You can close this tab and go back to Goose In A Pond.</p>"
                    .to_string(),
            )
            .into_response()
        }
        Err(reason) => {
            tracing::warn!(reason = %reason, "uber sign-in did not complete");
            oauth_callback::record_outcome(
                &state.oauth_outcomes,
                nonce,
                FlowOutcome::Failed(reason.clone()),
            )
            .await;
            Html(format!(
                "<h1>Uber is not connected</h1><p>{}</p>",
                crate::routes::html_escape(&reason)
            ))
            .into_response()
        }
    }
}
