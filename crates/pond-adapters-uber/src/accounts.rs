//! Members' Uber accounts: connecting through Jarida's sign-in relay, keeping each member's tokens
//! in the pond's secret store, and renewing them before they lapse. The relay adds the app's client
//! secret; the tokens themselves are kept only here.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use pond_core::security::ports::secret::SecretRepository;
use serde::Deserialize;
use serde_json::json;

use crate::UberAccessTokens;

/// The scopes a member grants: book and follow rides, read receipts, show which account is
/// connected, and keep the connection without signing in again.
pub const SCOPES: &[&str] = &["request", "request_receipt", "profile", "offline_access"];
pub const AUTHORIZE_URL: &str = "https://auth.uber.com/oauth/v2/authorize";

/// Renew this long before the access token lapses, so a booking never starts with a dying token.
const RENEW_MARGIN: chrono::Duration = chrono::Duration::minutes(10);
const RELAY_TIMEOUT: Duration = Duration::from_secs(15);
/// The name relay calls are filed under in the egress log, the same as the music-token fetch.
const EGRESS_TOOL: &str = "giap-credentials";

const ACCESS_KEY: &str = "UBER_ACCESS_TOKEN:";
const REFRESH_KEY: &str = "UBER_REFRESH_TOKEN:";
const EXPIRES_KEY: &str = "UBER_TOKEN_EXPIRES_AT:";

fn key(prefix: &str, profile_id: &str) -> String {
    format!("{prefix}{profile_id}")
}

/// Jarida's credentials service, which holds the Uber app's client secret.
pub struct SignInRelay {
    http: reqwest::Client,
    base_url: String,
}

/// What Uber returns for a sign-in or a renewal, as the relay passes it on.
#[derive(Debug, Deserialize)]
struct TokenSet {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
}

#[derive(Deserialize)]
struct RelayRefusal {
    uber_error: Option<String>,
    error: Option<String>,
}

impl SignInRelay {
    /// `base_url` is the credentials service's address (`POND_CREDENTIALS_URL`).
    pub fn new(http: reqwest::Client, base_url: &str) -> Self {
        Self {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }

    /// The Uber app's client ID. Public; Uber's sign-in page needs it.
    pub async fn client_id(&self) -> Result<String> {
        #[derive(Deserialize)]
        struct Client {
            client_id: String,
        }
        let url = format!("{}/v1/uber/client", self.base_url);
        let response = self.send(self.http.get(&url), &url, "GET").await?;
        let client: Client = response
            .json()
            .await
            .context("the credentials service's Uber client ID was not readable")?;
        Ok(client.client_id)
    }

    async fn exchange_code(&self, code: &str, redirect_uri: &str) -> Result<TokenSet> {
        self.tokens(
            "/v1/uber/token",
            json!({"code": code, "redirect_uri": redirect_uri}),
        )
        .await
    }

    async fn renew(&self, refresh_token: &str) -> Result<TokenSet> {
        self.tokens("/v1/uber/refresh", json!({"refresh_token": refresh_token}))
            .await
    }

    async fn tokens(&self, path: &str, body: serde_json::Value) -> Result<TokenSet> {
        let url = format!("{}{path}", self.base_url);
        let response = self
            .send(self.http.post(&url).json(&body), &url, "POST")
            .await?;
        response
            .json()
            .await
            .context("the credentials service's Uber tokens were not readable")
    }

    /// Gated by the egress guard, filed under the credentials service, and turned into an error
    /// that names Uber's own refusal code when there is one.
    async fn send(
        &self,
        builder: reqwest::RequestBuilder,
        url: &str,
        method: &'static str,
    ) -> Result<reqwest::Response> {
        let call = pond_core::shared::services::egress::begin_as(url, method, EGRESS_TOOL)
            .map_err(|denied| anyhow!("{denied}"))?;
        let sent = builder.timeout(RELAY_TIMEOUT).send().await;
        call.finish(sent.as_ref().ok().map(|r| r.status().as_u16()));
        let response = sent.map_err(|_| anyhow!("the credentials service could not be reached"))?;
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        let refusal: Option<RelayRefusal> = response.json().await.ok();
        let reason = refusal
            .and_then(|r| r.uber_error.or(r.error))
            .unwrap_or_else(|| status.to_string());
        bail!("Uber sign-in failed: {reason}")
    }
}

/// Each member's Uber connection, stored in the pond's secret store.
pub struct UberAccounts {
    secrets: Arc<dyn SecretRepository>,
    relay: SignInRelay,
    renewing: tokio::sync::Mutex<()>,
}

impl UberAccounts {
    pub fn new(secrets: Arc<dyn SecretRepository>, relay: SignInRelay) -> Self {
        Self {
            secrets,
            relay,
            renewing: tokio::sync::Mutex::new(()),
        }
    }

    pub fn relay(&self) -> &SignInRelay {
        &self.relay
    }

    /// Finish a member's sign-in: exchange Uber's code through the relay and keep the tokens.
    pub async fn connect(
        &self,
        profile_id: &str,
        code: &str,
        redirect_uri: &str,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let tokens = self.relay.exchange_code(code, redirect_uri).await?;
        let refresh = tokens
            .refresh_token
            .as_deref()
            .context("Uber gave no refresh token; was offline_access granted?")?;
        self.store(
            profile_id,
            &tokens.access_token,
            refresh,
            expiry(&tokens, now),
        )
        .await
    }

    /// Forget a member's Uber connection. Their Uber account itself is untouched.
    pub async fn disconnect(&self, profile_id: &str) -> Result<()> {
        for prefix in [ACCESS_KEY, REFRESH_KEY, EXPIRES_KEY] {
            self.secrets.delete(&key(prefix, profile_id)).await?;
        }
        Ok(())
    }

    /// The members who have connected Uber, by profile id.
    pub async fn connected_members(&self) -> Result<Vec<String>> {
        let mut members: Vec<String> = self
            .secrets
            .list_keys()
            .await?
            .into_iter()
            .filter_map(|k| k.strip_prefix(REFRESH_KEY).map(str::to_string))
            .collect();
        members.sort();
        Ok(members)
    }

    /// The member's access token, renewed first when it is about to lapse.
    pub async fn access_token_at(&self, profile_id: &str, now: DateTime<Utc>) -> Result<String> {
        if let Some(token) = self.fresh_access_token(profile_id, now).await? {
            return Ok(token);
        }
        let _one_at_a_time = self.renewing.lock().await;
        // Another caller may have renewed while this one waited.
        if let Some(token) = self.fresh_access_token(profile_id, now).await? {
            return Ok(token);
        }
        let refresh = self
            .get(REFRESH_KEY, profile_id)
            .await?
            .ok_or_else(|| anyhow!("this member has not connected Uber"))?;
        let tokens = self.relay.renew(&refresh).await?;
        // Uber may or may not rotate the refresh token; keep the old one when it doesn't.
        let next_refresh = tokens.refresh_token.clone().unwrap_or(refresh);
        self.store(
            profile_id,
            &tokens.access_token,
            &next_refresh,
            expiry(&tokens, now),
        )
        .await?;
        Ok(tokens.access_token)
    }

    async fn fresh_access_token(
        &self,
        profile_id: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<String>> {
        let Some(token) = self.get(ACCESS_KEY, profile_id).await? else {
            return Ok(None);
        };
        let expires_at = self
            .get(EXPIRES_KEY, profile_id)
            .await?
            .and_then(|v| v.parse::<i64>().ok())
            .and_then(|secs| DateTime::<Utc>::from_timestamp(secs, 0));
        Ok(match expires_at {
            Some(at) if at - RENEW_MARGIN > now => Some(token),
            _ => None,
        })
    }

    async fn store(
        &self,
        profile_id: &str,
        access: &str,
        refresh: &str,
        expires_at: DateTime<Utc>,
    ) -> Result<()> {
        self.secrets
            .set(&key(ACCESS_KEY, profile_id), access)
            .await?;
        self.secrets
            .set(&key(REFRESH_KEY, profile_id), refresh)
            .await?;
        self.secrets
            .set(
                &key(EXPIRES_KEY, profile_id),
                &expires_at.timestamp().to_string(),
            )
            .await
    }

    async fn get(&self, prefix: &str, profile_id: &str) -> Result<Option<String>> {
        Ok(self
            .secrets
            .get(&key(prefix, profile_id))
            .await?
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty()))
    }
}

/// When the access token lapses. Without `expires_in`, treat it as already lapsing so the next
/// use renews it rather than trusting a token of unknown life.
fn expiry(tokens: &TokenSet, now: DateTime<Utc>) -> DateTime<Utc> {
    tokens
        .expires_in
        .map(|secs| now + chrono::Duration::seconds(secs))
        .unwrap_or(now)
}

/// Uber's sign-in page for one attempt. `state` ties Uber's return to this attempt.
pub fn authorize_url(client_id: &str, redirect_uri: &str, state: &str) -> String {
    let scope = SCOPES.join(" ");
    format!(
        "{AUTHORIZE_URL}?client_id={}&response_type=code&redirect_uri={}&scope={}&state={}",
        encode(client_id),
        encode(redirect_uri),
        encode(&scope),
        encode(state),
    )
}

fn encode(s: &str) -> String {
    urlencoding::encode(s).into_owned()
}

#[async_trait]
impl UberAccessTokens for UberAccounts {
    async fn access_token(&self, profile_id: &str) -> Result<String> {
        self.access_token_at(profile_id, Utc::now()).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[derive(Default)]
    struct MemorySecrets(Mutex<HashMap<String, String>>);

    #[async_trait]
    impl SecretRepository for MemorySecrets {
        async fn get(&self, key: &str) -> Result<Option<String>> {
            Ok(self.0.lock().unwrap().get(key).cloned())
        }
        async fn set(&self, key: &str, value: &str) -> Result<()> {
            self.0
                .lock()
                .unwrap()
                .insert(key.to_string(), value.to_string());
            Ok(())
        }
        async fn delete(&self, key: &str) -> Result<()> {
            self.0.lock().unwrap().remove(key);
            Ok(())
        }
        async fn list_keys(&self) -> Result<Vec<String>> {
            Ok(self.0.lock().unwrap().keys().cloned().collect())
        }
        async fn has(&self, key: &str) -> Result<bool> {
            Ok(self.0.lock().unwrap().contains_key(key))
        }
    }

    const CALLBACK: &str = "http://127.0.0.1:4000/api/v1/oauth/callback";

    fn now() -> DateTime<Utc> {
        "2026-10-06T08:00:00Z".parse().unwrap()
    }

    fn accounts(server: &MockServer) -> (UberAccounts, Arc<MemorySecrets>) {
        let secrets = Arc::new(MemorySecrets::default());
        let accounts = UberAccounts::new(
            secrets.clone(),
            SignInRelay::new(reqwest::Client::new(), &server.uri()),
        );
        (accounts, secrets)
    }

    async fn relay_returns(server: &MockServer, route: &str, body: serde_json::Value) {
        Mock::given(method("POST"))
            .and(path(route))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn connecting_keeps_the_members_tokens_and_lists_them() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/uber/token"))
            .and(body_partial_json(
                json!({"code": "c-1", "redirect_uri": CALLBACK}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "a-1", "refresh_token": "r-1", "expires_in": 3600
            })))
            .expect(1)
            .mount(&server)
            .await;
        let (accounts, secrets) = accounts(&server);

        accounts
            .connect("liz", "c-1", CALLBACK, now())
            .await
            .unwrap();
        assert_eq!(
            secrets
                .get("UBER_REFRESH_TOKEN:liz")
                .await
                .unwrap()
                .as_deref(),
            Some("r-1")
        );
        assert_eq!(
            accounts.connected_members().await.unwrap(),
            vec!["liz".to_string()]
        );
        assert_eq!(accounts.access_token_at("liz", now()).await.unwrap(), "a-1");
    }

    #[tokio::test]
    async fn a_lapsing_token_is_renewed_and_a_rotated_refresh_token_kept() {
        let server = MockServer::start().await;
        relay_returns(
            &server,
            "/v1/uber/token",
            json!({
                "access_token": "a-1", "refresh_token": "r-1", "expires_in": 3600
            }),
        )
        .await;
        Mock::given(method("POST"))
            .and(path("/v1/uber/refresh"))
            .and(body_partial_json(json!({"refresh_token": "r-1"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "a-2", "refresh_token": "r-2", "expires_in": 3600
            })))
            .expect(1)
            .mount(&server)
            .await;
        let (accounts, secrets) = accounts(&server);
        accounts
            .connect("liz", "c-1", CALLBACK, now())
            .await
            .unwrap();

        // 55 minutes on, inside the renewal margin.
        let later = now() + chrono::Duration::minutes(55);
        assert_eq!(accounts.access_token_at("liz", later).await.unwrap(), "a-2");
        assert_eq!(
            secrets
                .get("UBER_REFRESH_TOKEN:liz")
                .await
                .unwrap()
                .as_deref(),
            Some("r-2")
        );
        // Fresh again: no second renewal (the mock expects exactly one).
        assert_eq!(accounts.access_token_at("liz", later).await.unwrap(), "a-2");
    }

    #[tokio::test]
    async fn a_renewal_without_a_new_refresh_token_keeps_the_old_one() {
        let server = MockServer::start().await;
        relay_returns(
            &server,
            "/v1/uber/token",
            json!({
                "access_token": "a-1", "refresh_token": "r-1", "expires_in": 60
            }),
        )
        .await;
        relay_returns(
            &server,
            "/v1/uber/refresh",
            json!({
                "access_token": "a-2", "expires_in": 3600
            }),
        )
        .await;
        let (accounts, secrets) = accounts(&server);
        accounts
            .connect("liz", "c-1", CALLBACK, now())
            .await
            .unwrap();
        assert_eq!(accounts.access_token_at("liz", now()).await.unwrap(), "a-2");
        assert_eq!(
            secrets
                .get("UBER_REFRESH_TOKEN:liz")
                .await
                .unwrap()
                .as_deref(),
            Some("r-1")
        );
    }

    #[tokio::test]
    async fn members_are_kept_apart_and_disconnecting_forgets_only_that_member() {
        let server = MockServer::start().await;
        relay_returns(
            &server,
            "/v1/uber/token",
            json!({
                "access_token": "a", "refresh_token": "r", "expires_in": 3600
            }),
        )
        .await;
        let (accounts, _) = accounts(&server);
        accounts.connect("liz", "c", CALLBACK, now()).await.unwrap();
        accounts
            .connect("jerry", "c", CALLBACK, now())
            .await
            .unwrap();

        accounts.disconnect("liz").await.unwrap();
        assert_eq!(
            accounts.connected_members().await.unwrap(),
            vec!["jerry".to_string()]
        );
        assert!(accounts.access_token_at("liz", now()).await.is_err());
        assert!(accounts.access_token_at("jerry", now()).await.is_ok());
    }

    #[tokio::test]
    async fn ubers_refusal_is_named_and_nothing_is_stored() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/uber/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                "error": "Uber refused the sign-in.", "uber_error": "invalid_grant"
            })))
            .mount(&server)
            .await;
        let (accounts, secrets) = accounts(&server);
        let err = accounts
            .connect("liz", "used", CALLBACK, now())
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("invalid_grant"), "{err:#}");
        assert!(secrets.list_keys().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_sign_in_without_offline_access_is_refused() {
        let server = MockServer::start().await;
        relay_returns(
            &server,
            "/v1/uber/token",
            json!({"access_token": "a", "expires_in": 3600}),
        )
        .await;
        let (accounts, secrets) = accounts(&server);
        assert!(accounts.connect("liz", "c", CALLBACK, now()).await.is_err());
        assert!(secrets.list_keys().await.unwrap().is_empty());
    }

    #[test]
    fn the_sign_in_page_asks_for_exactly_the_scopes_phase_2_uses() {
        let url = authorize_url("client-1", CALLBACK, "nonce-1");
        assert!(url.starts_with("https://auth.uber.com/oauth/v2/authorize?client_id=client-1"));
        assert!(url.contains("&response_type=code"));
        assert!(
            url.contains("&scope=request%20request_receipt%20profile%20offline_access"),
            "{url}"
        );
        assert!(url
            .contains("&redirect_uri=http%3A%2F%2F127.0.0.1%3A4000%2Fapi%2Fv1%2Foauth%2Fcallback"));
        assert!(url.ends_with("&state=nonce-1"));
    }
}
