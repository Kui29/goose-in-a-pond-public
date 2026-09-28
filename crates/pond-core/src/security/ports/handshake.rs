//! Driven Port: Handshake
//!
//! Device authentication and pairing between GIAP (server) and connecting
//! clients (the GOTG mobile app, other pond instances, the CLI, …).
//!
//! # Two-phase pairing protocol
//!
//! Pairing proves that the client holds a short-lived **pairing code** that the
//! operator read off this server's CLI/dashboard, without ever sending the code
//! over the wire:
//!
//! 1. `init_handshake(InitRequest) -> ChallengeResponse`
//!    The server mints a random 32-byte challenge bound to `client_id`,
//!    persists it with a short TTL, and returns it (base64) to the client.
//! 2. `verify_handshake(VerifyRequest)`
//!    The client computes a MAC over the challenge, keyed by the pairing code,
//!    and submits it. On success the server consumes the challenge + pairing
//!    code, registers the device, and mints a session+refresh token pair.
//!
//! # Channel binding
//!
//! A client that reached this server over pinned TLS names the key it pinned to
//! in [`VerifyRequest::channel_binding`] and folds it into the MAC. The server
//! recomputes with **its own** key, so the two agree only when the client is
//! talking to this server directly:
//!
//! ```text
//! bound   mac = HMAC(code, "goose-pair-client-v1\0" || challenge || \0 || client_id || \0 || spki)
//! unbound mac = HMAC(code, challenge || client_id)
//! ```
//!
//! and the server answers with [`HandshakeResponse::server_proof`] over the same
//! transcript under `goose-pair-server-v1`, which only something holding the
//! pairing code can produce.
//!
//! What this buys: the pin no longer has to be carried to the phone by a
//! trustworthy route. Somebody who intercepts the connection and presents their
//! own certificate -- by answering an mDNS query, say -- gets a client that
//! MACs over *their* key. Relaying that to this server fails the recomputation;
//! stripping the binding and relaying leaves a MAC over a transcript this
//! server no longer computes; and answering the client themselves fails the
//! server proof. A wrong pin therefore ends pairing in a visible failure
//! instead of a successful pair with the wrong pond.
//!
//! The binding is optional because one real caller has no channel to bind: the
//! desktop dashboard pairs over loopback HTTP, where there is no certificate
//! and no interceptor. Optional does not mean downgradable -- a client that
//! binds always binds, and nobody in the middle can compute the unbound MAC
//! either, because both forms need the pairing code.
//!
//! `refresh` rotates an expiring session token; `revoke_token` disconnects a
//! client. Pairing codes are issued by the server via `issue_pairing_code`
//! (shown on the CLI/dashboard) and are single-use.
//!
//! The legacy single-shot `handshake()` method is retained for the in-memory
//! `MockHandshake` (tests) and for already-paired clients that present a
//! pairing code directly.

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Request from a client wanting to connect to GIAP (legacy single-shot).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandshakeRequest {
    /// Client identifier (e.g. mobile device UUID).
    pub client_id: String,
    /// Client type: "gotg", "pond", "cli", etc.
    pub client_type: String,
    pub client_version: String,
    pub pairing_code: Option<String>,
}

/// Response from GIAP after a handshake / refresh.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandshakeResponse {
    pub accepted: bool,
    /// Session token for subsequent API calls (`Authorization: Bearer …`).
    pub session_token: Option<String>,
    /// Refresh token — populated by the two-phase / refresh paths only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// RFC3339 expiry of the session token. Absent for legacy responses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    pub hostname: String,
    pub server_version: String,
    pub capabilities: Vec<String>,
    pub rejection_reason: Option<String>,
    /// Hex `HMAC-SHA256` proving this server holds the pairing code and serves
    /// the certificate the client pinned. See the channel-binding note above.
    ///
    /// Present exactly when the accepted request carried a
    /// [`VerifyRequest::channel_binding`]. A client that sent one and did not
    /// get one back is not talking to the pond it thinks it is, and must treat
    /// the pair as failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_proof: Option<String>,
}

/// Phase 1 request: the client asks for a challenge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitRequest {
    pub client_id: String,
    pub client_type: String,
    pub client_version: String,
}

/// Phase 1 response: the challenge the client must MAC.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChallengeResponse {
    pub challenge_id: String,
    /// Base64-encoded random challenge bytes.
    pub challenge: String,
    /// RFC3339 expiry of the challenge.
    pub expires_at: String,
}

/// Phase 2 request: the client proves possession of the pairing code.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyRequest {
    pub challenge_id: String,
    /// Hex-encoded MAC over the transcript named by `channel_binding`.
    pub mac: String,
    /// Optional friendly device name to record in the devices table.
    #[serde(default)]
    pub device_name: Option<String>,
    /// The server public-key pin this client pinned its connection to, in
    /// `sha256/<base64>` form, when it reached the server over pinned TLS.
    ///
    /// `None` means there was no channel to bind -- the loopback dashboard --
    /// and selects the unbound MAC. Anything else must equal this server's own
    /// pin, or the request is rejected: see the channel-binding note above.
    #[serde(default)]
    pub channel_binding: Option<String>,
}

/// Exchange a refresh token for a fresh session+refresh pair.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefreshRequest {
    pub refresh_token: String,
}

/// A server-issued pairing code, shown on the CLI/dashboard.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairingCode {
    /// 6-digit code shown to the operator (leading zeros preserved).
    pub code: String,
    /// RFC3339 expiry of the code.
    pub expires_at: String,
    /// Member the device paired with this code becomes; `None` pairs an unattributed device.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
}

/// Who a valid session token was issued to.
/// The ids match today, but identity must follow `device_id`: `client_id` is self-reported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenCaller {
    pub client_id: String,
    /// The `devices` row this token's pairing registered.
    pub device_id: String,
}

/// Driven port: device authentication and pairing.
#[async_trait]
pub trait Handshake: Send + Sync {
    /// Legacy single-shot handshake; real clients should prefer `init`/`verify`.
    async fn handshake(&self, request: HandshakeRequest) -> Result<HandshakeResponse>;

    /// Validate an existing session token.
    async fn validate_token(&self, token: &str) -> Result<bool>;

    /// The client and device a valid token was issued to, if this adapter can say.
    /// Sole source of the paired-device identity rung, which outranks face and explicit, so the
    /// device id must never come from request input. The `Ok(None)` default only skips that rung.
    async fn caller_for_token(&self, _token: &str) -> Result<Option<TokenCaller>> {
        Ok(None)
    }

    /// The `client_id` a valid token was issued to, if known; derived so the two cannot disagree.
    async fn client_id_for_token(&self, token: &str) -> Result<Option<String>> {
        Ok(self
            .caller_for_token(token)
            .await?
            .map(|caller| caller.client_id))
    }

    /// Revoke every session a device holds, and report how many were live.
    ///
    /// Removing a device from the household registry has to take its access
    /// with it. `session_tokens.device_id` carries no foreign key, and nothing
    /// cascades onto that table, so deleting the `devices` row on its own left
    /// the tokens valid -- an operator who removed a lost phone from the device
    /// list would have been told it was gone while it carried on working.
    ///
    /// # Why this has no default
    ///
    /// Every other new method on this trait is defaulted, and each of those
    /// defaults **narrows**: a forgotten override loses a capability. A default
    /// here would do the opposite. `Ok(0)` would mean "revoked nothing", the
    /// caller would delete the device row anyway, and the omission would widen
    /// access while reading like success. So it is required, and an adapter
    /// that cannot revoke has to say so out loud.
    async fn revoke_device(&self, device_id: &str) -> Result<u64>;

    /// Revoke a session token (disconnect a client).
    async fn revoke_token(&self, token: &str) -> Result<()>;

    /// Phase 1: issue a challenge bound to a `client_id`.
    async fn init_handshake(&self, _request: InitRequest) -> Result<ChallengeResponse> {
        Err(anyhow::anyhow!(
            "two-phase handshake not supported by this adapter"
        ))
    }

    /// Phase 2: verify the client's MAC and mint tokens.
    async fn verify_handshake(&self, _request: VerifyRequest) -> Result<HandshakeResponse> {
        Err(anyhow::anyhow!(
            "two-phase handshake not supported by this adapter"
        ))
    }

    /// Exchange a refresh token for a fresh session+refresh pair.
    async fn refresh(&self, _request: RefreshRequest) -> Result<HandshakeResponse> {
        Err(anyhow::anyhow!("refresh not supported by this adapter"))
    }

    /// Issue a single-use pairing code bound to a member; returns the only plaintext copy.
    /// Bound on the host, never by the pairing client: the paired-device rung outranks all proofs.
    async fn issue_pairing_code_for(&self, _profile_id: Option<&str>) -> Result<PairingCode> {
        Err(anyhow::anyhow!(
            "pairing-code issuance not supported by this adapter"
        ))
    }

    /// Issue an unattributed single-use pairing code; adapters implement `issue_pairing_code_for`.
    async fn issue_pairing_code(&self) -> Result<PairingCode> {
        self.issue_pairing_code_for(None).await
    }

    /// The latest unexpired, unconsumed pairing code, if any, for the dashboard to re-display.
    async fn current_pairing_code(&self) -> Result<Option<PairingCode>> {
        Ok(None)
    }
}
