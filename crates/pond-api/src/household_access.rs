//! Resolves who is calling for `pond_core::user_data::services::household_access` and turns its
//! refusals into the one 403 shape every gated route answers with.

use std::convert::Infallible;
use std::sync::Arc;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use pond_core::security::domain::proven_device::ProvenDevice;
use pond_core::security::ports::policy::{Principal, PrincipalKind};
use pond_core::user_data::ports::device_attribution::DeviceAttribution;
use pond_core::user_data::services::household_access::{
    may, Action, Caller, Refusal, ROLE_REQUIRED,
};
use serde_json::{json, Value};

use crate::host_guard::HostCredential;
use crate::AppState;

/// Who is asking. Stores are read only when the host credential and onboarding have not answered.
pub async fn caller(
    state: &AppState,
    principal: Option<&Principal>,
    credential: Option<&HostCredential>,
    headers: &HeaderMap,
) -> Caller {
    // A loopback principal is the opt-in dev bypass or host recovery: both are the host.
    let loopback = principal.is_some_and(|p| p.kind == PrincipalKind::Loopback);
    if loopback || crate::host_guard::presents_credential(credential, headers) {
        return Caller::Host;
    }
    let device = principal.map_or_else(ProvenDevice::none, ProvenDevice::from_principal);
    let Some(device_id) = device.id() else {
        // Before onboarding the middleware admits a tokenless caller only from the pond's own host.
        return if crate::middleware::pond_is_onboarded(state).await {
            Caller::NoDevice
        } else {
            Caller::Host
        };
    };
    let attribution = pond_infra::sqlite_device_attribution::SqliteDeviceAttribution::new(
        state.db.system.clone(),
    )
    .device_profile(device_id)
    .await;
    match attribution {
        Ok(Some(profile_id)) if !profile_id.trim().is_empty() => {
            match state.profile_repo.get(&profile_id).await {
                Ok(Some(profile)) => Caller::Member {
                    profile_id,
                    role: profile.role,
                },
                Ok(None) => Caller::Unattributed,
                Err(error) => {
                    tracing::warn!(%error, "household access: the member's profile is unreadable");
                    Caller::Unreadable
                }
            }
        }
        Ok(_) if is_claimant(state, device_id).await => Caller::Claimant,
        Ok(_) if !crate::middleware::pond_is_onboarded(state).await => Caller::Setup,
        Ok(_) => Caller::Unattributed,
        Err(error) => {
            tracing::warn!(%error, "household access: the device's member is unreadable");
            Caller::Unreadable
        }
    }
}

async fn is_claimant(state: &AppState, device_id: &str) -> bool {
    let Some(claims) = &state.setup_claims else {
        return false;
    };
    match claims.claimed_by().await {
        Ok(claimant) => claimant.as_deref() == Some(device_id),
        Err(error) => {
            tracing::warn!(%error, "household access: the claim is unreadable; nobody counts as its claimant");
            false
        }
    }
}

/// The 403 a gated route answers with.
pub type Refused = (StatusCode, Json<Value>);

/// The household caller as an extractor, so a gated handler takes one argument.
pub struct Household {
    pub caller: Caller,
    device_id: Option<String>,
}

impl FromRequestParts<Arc<AppState>> for Household {
    type Rejection = Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        let principal = parts.extensions.get::<Principal>();
        let credential = parts.extensions.get::<HostCredential>();
        Ok(Self {
            caller: caller(state, principal, credential, &parts.headers).await,
            device_id: principal.and_then(|p| p.device_id.clone()),
        })
    }
}

impl Household {
    pub fn require(&self, action: Action) -> Result<(), Refused> {
        may(&self.caller, action).map_err(|refusal| self.refused(&refusal))
    }

    /// The device this request came from, if any.
    pub fn device_id(&self) -> Option<&str> {
        self.device_id.as_deref()
    }

    pub fn refused(&self, refusal: &Refusal) -> Refused {
        refused(self.device_id(), refusal)
    }
}

/// Logs the refusal and builds its 403.
pub fn refused(device_id: Option<&str>, refusal: &Refusal) -> Refused {
    tracing::warn!(
        kind = "household_access_denied",
        action = ?refusal.action,
        role = ?refusal.role,
        caller = device_id.unwrap_or("none"),
        "refused a household change for this caller's role"
    );
    (
        StatusCode::FORBIDDEN,
        Json(json!({
            "error": ROLE_REQUIRED,
            "action": refusal.action,
            "role": refusal.role,
            "allowed": refusal.allowed,
            "reason": refusal.to_string(),
        })),
    )
}
