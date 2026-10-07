//! Uber sign-in relay. Uber's token endpoint needs Jarida's client secret for every code exchange
//! and refresh, and a secret cannot ship in every pond. So the pond sends the code (or refresh
//! token) here, this adds the secret, and Uber's tokens go straight back. Nothing is stored or
//! logged: the member's tokens live only on their own pond.

use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};

/// Uber's documented token endpoint.
pub const DEFAULT_TOKEN_URL: &str = "https://auth.uber.com/oauth/v2/token";

const UBER_TIMEOUT: Duration = Duration::from_secs(15);

/// The only return address a pond's sign-in uses: its own loopback OAuth callback. Refusing
/// anything else keeps the relay from exchanging codes issued to some other site.
const POND_CALLBACK_HOST: &str = "http://127.0.0.1:";
const POND_CALLBACK_PATH: &str = "/api/v1/oauth/callback";

#[derive(Clone)]
pub struct UberConfig {
    pub client_id: String,
    pub client_secret: String,
    pub token_url: String,
}

impl UberConfig {
    /// `None` when `UBER_CLIENT_ID` is unset: the relay is optional and the service still serves
    /// MusicKit tokens. The secret comes from a file, like the Apple key, never a variable.
    pub fn from_env(
        get: &impl Fn(&str) -> Option<String>,
        read_file: &impl Fn(&str) -> Result<String, String>,
    ) -> Result<Option<UberConfig>, String> {
        let value = |name: &str| {
            get(name)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let Some(client_id) = value("UBER_CLIENT_ID") else {
            return Ok(None);
        };
        let path = value("UBER_CLIENT_SECRET_FILE")
            .ok_or("UBER_CLIENT_SECRET_FILE is not set, but UBER_CLIENT_ID is.")?;
        let client_secret = read_file(&path)
            .map_err(|e| format!("cannot read UBER_CLIENT_SECRET_FILE: {e}"))?
            .trim()
            .to_string();
        if client_secret.is_empty() {
            return Err("UBER_CLIENT_SECRET_FILE is empty.".into());
        }
        Ok(Some(UberConfig {
            client_id,
            client_secret,
            token_url: value("UBER_TOKEN_URL").unwrap_or_else(|| DEFAULT_TOKEN_URL.to_string()),
        }))
    }
}

pub struct UberRelay {
    config: UberConfig,
    http: reqwest::Client,
}

/// How a relayed exchange ended, ready to become a response.
#[derive(Debug, PartialEq)]
pub enum Relayed {
    /// Uber's tokens, reduced to the fields a pond uses.
    Tokens(Value),
    /// Uber refused: its own error code (`invalid_grant`), never its whole reply.
    Refused(String),
    /// The pond sent something this relay will not forward.
    BadRequest(&'static str),
    /// Uber could not be reached or answered with something that isn't tokens.
    Unavailable,
}

#[derive(Deserialize)]
pub struct CodeExchange {
    pub code: String,
    pub redirect_uri: String,
}

#[derive(Deserialize)]
pub struct Refresh {
    pub refresh_token: String,
}

impl UberRelay {
    pub fn new(config: &UberConfig) -> Result<UberRelay, String> {
        let http = reqwest::Client::builder()
            .timeout(UBER_TIMEOUT)
            .build()
            .map_err(|e| format!("cannot build the HTTP client: {e}"))?;
        Ok(UberRelay {
            config: config.clone(),
            http,
        })
    }

    /// Not secret: the pond needs it to build Uber's sign-in address.
    pub fn client_id(&self) -> &str {
        &self.config.client_id
    }

    pub async fn exchange_code(&self, request: &CodeExchange) -> Relayed {
        if request.code.trim().is_empty() {
            return Relayed::BadRequest("code is empty");
        }
        if !is_pond_callback(&request.redirect_uri) {
            return Relayed::BadRequest("redirect_uri is not a pond's sign-in callback");
        }
        self.post(&[
            ("grant_type", "authorization_code"),
            ("code", request.code.trim()),
            ("redirect_uri", request.redirect_uri.as_str()),
        ])
        .await
    }

    pub async fn refresh(&self, request: &Refresh) -> Relayed {
        if request.refresh_token.trim().is_empty() {
            return Relayed::BadRequest("refresh_token is empty");
        }
        self.post(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", request.refresh_token.trim()),
        ])
        .await
    }

    async fn post(&self, grant: &[(&str, &str)]) -> Relayed {
        let mut form = vec![
            ("client_id", self.config.client_id.as_str()),
            ("client_secret", self.config.client_secret.as_str()),
        ];
        form.extend_from_slice(grant);

        let response = match self
            .http
            .post(&self.config.token_url)
            .form(&form)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                // reqwest's error names the URL, never the form, so it carries no secret.
                tracing::warn!(error = %e.without_url(), "could not reach Uber's token endpoint");
                return Relayed::Unavailable;
            }
        };
        let status = response.status();
        let body: Value = response.json().await.unwrap_or(Value::Null);
        if status.is_success() {
            return tokens(&body).map_or(Relayed::Unavailable, Relayed::Tokens);
        }
        let code = body
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("refused")
            .to_string();
        tracing::info!(status = status.as_u16(), %code, "Uber refused a token request");
        Relayed::Refused(code)
    }
}

/// `http://127.0.0.1:<port>/api/v1/oauth/callback` and nothing else.
fn is_pond_callback(uri: &str) -> bool {
    let Some(rest) = uri.strip_prefix(POND_CALLBACK_HOST) else {
        return false;
    };
    let Some(port) = rest.strip_suffix(POND_CALLBACK_PATH) else {
        return false;
    };
    port.parse::<u16>().is_ok_and(|p| p > 0)
}

/// The fields a pond uses, so anything else Uber adds never passes through this service.
fn tokens(body: &Value) -> Option<Value> {
    let access_token = body.get("access_token")?.as_str()?;
    Some(json!({
        "access_token": access_token,
        "refresh_token": body.get("refresh_token").and_then(Value::as_str),
        "expires_in": body.get("expires_in").and_then(Value::as_u64),
        "scope": body.get("scope").and_then(Value::as_str),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_ponds_loopback_callback_is_accepted() {
        assert!(is_pond_callback(
            "http://127.0.0.1:4000/api/v1/oauth/callback"
        ));
        assert!(is_pond_callback(
            "http://127.0.0.1:8080/api/v1/oauth/callback"
        ));
        for bad in [
            "https://evil.example/api/v1/oauth/callback",
            "http://127.0.0.1.evil.example:4000/api/v1/oauth/callback",
            "http://127.0.0.1:4000/api/v1/oauth/callback?x=1",
            "http://127.0.0.1:0/api/v1/oauth/callback",
            "http://127.0.0.1:/api/v1/oauth/callback",
            "http://localhost:4000/api/v1/oauth/callback",
        ] {
            assert!(!is_pond_callback(bad), "{bad} accepted");
        }
    }

    #[test]
    fn only_the_token_fields_pass_through() {
        let out = tokens(&json!({
            "access_token": "a", "refresh_token": "r", "expires_in": 2592000,
            "scope": "request profile", "token_type": "Bearer", "last_authenticated": 0
        }))
        .unwrap();
        assert_eq!(
            out,
            json!({"access_token": "a", "refresh_token": "r", "expires_in": 2592000, "scope": "request profile"})
        );
        assert_eq!(tokens(&json!({"error": "invalid_grant"})), None);
    }

    #[test]
    fn the_relay_is_optional_but_complete_when_configured() {
        let none = |_: &str| None::<String>;
        let read = |_: &str| Ok::<_, String>("secret".to_string());
        assert!(UberConfig::from_env(&none, &read).unwrap().is_none());

        let id_only = |name: &str| (name == "UBER_CLIENT_ID").then(|| "client".to_string());
        assert!(matches!(
            UberConfig::from_env(&id_only, &read),
            Err(e) if e.contains("UBER_CLIENT_SECRET_FILE")
        ));

        let both = |name: &str| match name {
            "UBER_CLIENT_ID" => Some("client".to_string()),
            "UBER_CLIENT_SECRET_FILE" => Some("/run/secrets/uber".to_string()),
            _ => None,
        };
        let config = UberConfig::from_env(&both, &read).unwrap().unwrap();
        assert_eq!(config.client_secret, "secret");
        assert_eq!(config.token_url, DEFAULT_TOKEN_URL);

        let empty = |_: &str| Ok::<_, String>("  \n".to_string());
        assert!(UberConfig::from_env(&both, &empty).is_err());
    }
}
