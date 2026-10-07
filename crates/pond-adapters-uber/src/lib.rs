//! Uber's Riders API (v1.2) as a [`RideProvider`]: upfront fares, ride requests, status and
//! cancellation, each on the member's own Uber account. The `request` scope is privileged, so
//! production use needs Uber's Full Access approval; the sandbox works without it.

pub mod accounts;

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use pond_core::rides::domain::{Driver, FareQuote, Place, Ride, RideStatus, Vehicle};
use pond_core::rides::ports::RideProvider;
use serde::Deserialize;
use serde_json::{json, Value};

/// Uber's production API; [`SANDBOX_API_BASE`] simulates rides without dispatching a car.
pub const PRODUCTION_API_BASE: &str = "https://api.uber.com";
pub const SANDBOX_API_BASE: &str = "https://sandbox-api.uber.com";

const API_BASE_ENV: &str = "GIAP_UBER_API_BASE";
const PRODUCT_ID_ENV: &str = "GIAP_UBER_PRODUCT_ID";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// Where to reach Uber and which product to ask for.
#[derive(Debug, Clone, PartialEq)]
pub struct UberConfig {
    pub api_base: String,
    /// `None` lets Uber pick its default product for the pickup.
    pub product_id: Option<String>,
}

impl UberConfig {
    /// From `GIAP_UBER_API_BASE` (default: production) and `GIAP_UBER_PRODUCT_ID`.
    pub fn from_env() -> Self {
        Self::from_values(
            std::env::var(API_BASE_ENV).ok(),
            std::env::var(PRODUCT_ID_ENV).ok(),
        )
    }

    fn from_values(api_base: Option<String>, product_id: Option<String>) -> Self {
        let non_blank =
            |v: Option<String>| v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        Self {
            api_base: non_blank(api_base)
                .unwrap_or_else(|| PRODUCTION_API_BASE.to_string())
                .trim_end_matches('/')
                .to_string(),
            product_id: non_blank(product_id),
        }
    }
}

/// A member's Uber access token. Implemented by the sign-in flow that stores it.
#[async_trait]
pub trait UberAccessTokens: Send + Sync {
    /// The member's current token; an error when they have not connected Uber.
    async fn access_token(&self, profile_id: &str) -> Result<String>;
}

pub struct UberRides {
    client: reqwest::Client,
    config: UberConfig,
    tokens: Arc<dyn UberAccessTokens>,
}

impl UberRides {
    pub fn new(
        client: reqwest::Client,
        config: UberConfig,
        tokens: Arc<dyn UberAccessTokens>,
    ) -> Self {
        Self {
            client,
            config,
            tokens,
        }
    }

    /// One authenticated call. Gated and recorded by the egress guard like every outbound call.
    async fn call(
        &self,
        profile_id: &str,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Option<Value>> {
        let url = format!("{}{path}", self.config.api_base);
        pond_core::shared::services::egress::check_egress(&url)?;
        let token = self.tokens.access_token(profile_id).await?;

        let mut builder = self
            .client
            .request(method.clone(), &url)
            .bearer_auth(token)
            .timeout(REQUEST_TIMEOUT);
        if let Some(body) = body {
            builder = builder.json(&body);
        }
        let start = Instant::now();
        let result = builder.send().await;
        pond_core::shared::services::egress::record_egress(
            &url,
            method.as_str(),
            result.as_ref().ok().map(|r| r.status().as_u16()),
            start.elapsed().as_millis() as u64,
        );
        let response = result.with_context(|| format!("Uber {method} {path} failed"))?;

        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!(
                "Uber refused {method} {path}: {}",
                refusal(status, &text)
            ));
        }
        if text.trim().is_empty() {
            return Ok(None);
        }
        serde_json::from_str(&text)
            .map(Some)
            .with_context(|| format!("Uber {method} {path} returned something other than JSON"))
    }

    fn trip_body(&self, pickup: &Place, dropoff: &Place) -> Value {
        let mut body = json!({
            "start_latitude": pickup.latitude,
            "start_longitude": pickup.longitude,
            "end_latitude": dropoff.latitude,
            "end_longitude": dropoff.longitude,
        });
        if let Some(product) = &self.config.product_id {
            body["product_id"] = json!(product);
        }
        body
    }
}

#[async_trait]
impl RideProvider for UberRides {
    fn name(&self) -> &'static str {
        "uber"
    }

    async fn quote(&self, profile_id: &str, pickup: &Place, dropoff: &Place) -> Result<FareQuote> {
        let body = self.trip_body(pickup, dropoff);
        let value = self
            .call(
                profile_id,
                reqwest::Method::POST,
                "/v1.2/requests/estimate",
                Some(body),
            )
            .await?
            .context("Uber returned an empty fare estimate")?;
        let estimate: EstimateResponse = serde_json::from_value(value)
            .context("Uber's fare estimate had an unexpected shape")?;
        // Without an upfront fare there is no fare_id, and a ride cannot be requested at a price.
        let fare = estimate
            .fare
            .context("Uber gave no upfront fare for this trip, so no price can be confirmed")?;
        Ok(FareQuote {
            fare_id: fare.fare_id,
            display: fare.display,
            currency_code: fare.currency_code,
            expires_at: DateTime::<Utc>::from_timestamp(fare.expires_at, 0)
                .context("Uber's fare expiry is not a valid time")?,
            pickup_eta_mins: estimate.pickup_estimate,
            product_id: self.config.product_id.clone(),
        })
    }

    async fn request(
        &self,
        profile_id: &str,
        pickup: &Place,
        dropoff: &Place,
        quote: &FareQuote,
    ) -> Result<Ride> {
        let mut body = self.trip_body(pickup, dropoff);
        body["fare_id"] = json!(quote.fare_id);
        body["start_nickname"] = json!(pickup.name);
        body["end_nickname"] = json!(dropoff.name);
        let value = self
            .call(
                profile_id,
                reqwest::Method::POST,
                "/v1.2/requests",
                Some(body),
            )
            .await?
            .context("Uber accepted the request but returned no ride")?;
        parse_ride(value)
    }

    async fn ride(&self, profile_id: &str, request_id: &str) -> Result<Ride> {
        let path = format!("/v1.2/requests/{request_id}");
        let value = self
            .call(profile_id, reqwest::Method::GET, &path, None)
            .await?
            .context("Uber returned no ride")?;
        parse_ride(value)
    }

    async fn cancel(&self, profile_id: &str, request_id: &str) -> Result<()> {
        let path = format!("/v1.2/requests/{request_id}");
        self.call(profile_id, reqwest::Method::DELETE, &path, None)
            .await
            .map(|_| ())
    }
}

// ── Uber's JSON ───────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct EstimateResponse {
    fare: Option<UberFare>,
    pickup_estimate: Option<u32>,
}

#[derive(Deserialize)]
struct UberFare {
    fare_id: String,
    display: String,
    currency_code: String,
    /// Unix seconds.
    expires_at: i64,
}

#[derive(Deserialize)]
struct UberRide {
    request_id: String,
    status: String,
    driver: Option<UberDriver>,
    vehicle: Option<UberVehicle>,
    pickup: Option<UberStop>,
    /// The create response carries the pickup ETA here instead of under `pickup`.
    eta: Option<u32>,
}

#[derive(Deserialize)]
struct UberDriver {
    name: String,
    phone_number: Option<String>,
    rating: Option<f32>,
}

#[derive(Deserialize)]
struct UberVehicle {
    make: String,
    model: String,
    license_plate: String,
}

#[derive(Deserialize)]
struct UberStop {
    eta: Option<u32>,
}

fn parse_ride(value: Value) -> Result<Ride> {
    let ride: UberRide =
        serde_json::from_value(value).context("Uber's ride had an unexpected shape")?;
    Ok(Ride {
        request_id: ride.request_id,
        status: parse_status(&ride.status),
        driver: ride.driver.map(|d| Driver {
            name: d.name,
            phone_number: d.phone_number,
            rating: d.rating,
        }),
        vehicle: ride.vehicle.map(|v| Vehicle {
            make: v.make,
            model: v.model,
            license_plate: v.license_plate,
        }),
        pickup_eta_mins: ride.pickup.and_then(|p| p.eta).or(ride.eta),
    })
}

fn parse_status(raw: &str) -> RideStatus {
    match raw {
        "processing" => RideStatus::Processing,
        "no_drivers_available" => RideStatus::NoDriversAvailable,
        "accepted" => RideStatus::Accepted,
        "arriving" => RideStatus::Arriving,
        "in_progress" => RideStatus::InProgress,
        "driver_canceled" => RideStatus::DriverCanceled,
        "rider_canceled" => RideStatus::RiderCanceled,
        "completed" => RideStatus::Completed,
        other => RideStatus::Other(other.to_string()),
    }
}

/// Uber's own error code and message when it sent them (v1 `code`/`message`, v1.2 `errors[]`),
/// else just the HTTP status. Never the raw body, which could carry trip details.
fn refusal(status: reqwest::StatusCode, body: &str) -> String {
    let parsed: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let first = parsed
        .get("errors")
        .and_then(|e| e.get(0))
        .unwrap_or(&parsed);
    let code = first.get("code").and_then(Value::as_str);
    let message = first
        .get("message")
        .or_else(|| first.get("title"))
        .and_then(Value::as_str);
    match (code, message) {
        (Some(c), Some(m)) => format!("{status} {c}: {m}"),
        (Some(c), None) => format!("{status} {c}"),
        (None, Some(m)) => format!("{status}: {m}"),
        (None, None) => status.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    struct FixedToken;

    #[async_trait]
    impl UberAccessTokens for FixedToken {
        async fn access_token(&self, profile_id: &str) -> Result<String> {
            match profile_id {
                "liz" => Ok("token-liz".to_string()),
                _ => Err(anyhow!("this member has not connected Uber")),
            }
        }
    }

    fn uber(server: &MockServer, product_id: Option<&str>) -> UberRides {
        UberRides::new(
            reqwest::Client::new(),
            UberConfig {
                api_base: server.uri(),
                product_id: product_id.map(str::to_string),
            },
            Arc::new(FixedToken),
        )
    }

    fn home() -> Place {
        Place {
            name: "Home".to_string(),
            latitude: -1.2676,
            longitude: 36.8108,
        }
    }

    fn jkia() -> Place {
        Place {
            name: "JKIA".to_string(),
            latitude: -1.319167,
            longitude: 36.9275,
        }
    }

    fn quote() -> FareQuote {
        FareQuote {
            fare_id: "fare-123".to_string(),
            display: "KES 1,250".to_string(),
            currency_code: "KES".to_string(),
            expires_at: DateTime::<Utc>::from_timestamp(1_790_000_000, 0).unwrap(),
            pickup_eta_mins: Some(3),
            product_id: None,
        }
    }

    #[test]
    fn config_defaults_to_production_and_ignores_blanks() {
        assert_eq!(
            UberConfig::from_values(None, Some("  ".into())),
            UberConfig {
                api_base: PRODUCTION_API_BASE.to_string(),
                product_id: None
            }
        );
        assert_eq!(
            UberConfig::from_values(Some(format!("{SANDBOX_API_BASE}/")), Some("uberx".into())),
            UberConfig {
                api_base: SANDBOX_API_BASE.to_string(),
                product_id: Some("uberx".into())
            }
        );
    }

    #[tokio::test]
    async fn quote_reads_the_upfront_fare_with_the_members_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1.2/requests/estimate"))
            .and(header("authorization", "Bearer token-liz"))
            .and(body_partial_json(json!({
                "start_latitude": -1.2676, "end_latitude": -1.319167, "product_id": "uberx"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "fare": {
                    "value": 1250.0, "fare_id": "fare-123", "expires_at": 1_790_000_000,
                    "display": "KES 1,250", "currency_code": "KES"
                },
                "trip": {"distance_unit": "km", "duration_estimate": 1500, "distance_estimate": 16.2},
                "pickup_estimate": 3
            })))
            .expect(1)
            .mount(&server)
            .await;

        let q = uber(&server, Some("uberx"))
            .quote("liz", &home(), &jkia())
            .await
            .unwrap();
        assert_eq!(q.fare_id, "fare-123");
        assert_eq!(q.display, "KES 1,250");
        assert_eq!(q.expires_at.timestamp(), 1_790_000_000);
        assert_eq!(q.pickup_eta_mins, Some(3));
    }

    #[tokio::test]
    async fn an_estimate_without_an_upfront_fare_is_an_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1.2/requests/estimate"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "estimate": {"display": "KES 1,100-1,400"}, "pickup_estimate": 3
            })))
            .mount(&server)
            .await;
        let err = uber(&server, None)
            .quote("liz", &home(), &jkia())
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("no upfront fare"), "{err:#}");
    }

    #[tokio::test]
    async fn request_sends_the_fare_id_and_reads_the_ride() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1.2/requests"))
            .and(body_partial_json(
                json!({"fare_id": "fare-123", "end_nickname": "JKIA"}),
            ))
            .respond_with(ResponseTemplate::new(202).set_body_json(json!({
                "request_id": "req-9", "status": "processing", "eta": 5,
                "driver": null, "vehicle": null, "location": null, "surge_multiplier": 1.0
            })))
            .expect(1)
            .mount(&server)
            .await;
        let ride = uber(&server, None)
            .request("liz", &home(), &jkia(), &quote())
            .await
            .unwrap();
        assert_eq!(ride.request_id, "req-9");
        assert_eq!(ride.status, RideStatus::Processing);
        assert_eq!(ride.pickup_eta_mins, Some(5));
    }

    #[tokio::test]
    async fn ride_reads_driver_vehicle_and_pickup_eta() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1.2/requests/req-9"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "request_id": "req-9", "status": "accepted",
                "driver": {"name": "Amina", "phone_number": "+254700000000", "rating": 4.9},
                "vehicle": {"make": "Toyota", "model": "Axio", "license_plate": "KDA 123A"},
                "pickup": {"latitude": -1.2676, "longitude": 36.8108, "eta": 4}
            })))
            .mount(&server)
            .await;
        let ride = uber(&server, None).ride("liz", "req-9").await.unwrap();
        assert_eq!(ride.status, RideStatus::Accepted);
        assert_eq!(ride.driver.unwrap().name, "Amina");
        assert_eq!(ride.vehicle.unwrap().license_plate, "KDA 123A");
        assert_eq!(ride.pickup_eta_mins, Some(4));
    }

    #[tokio::test]
    async fn cancel_deletes_the_request() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/v1.2/requests/req-9"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        uber(&server, None).cancel("liz", "req-9").await.unwrap();
    }

    #[tokio::test]
    async fn a_refusal_names_ubers_code_but_not_the_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1.2/requests"))
            .respond_with(ResponseTemplate::new(409).set_body_json(json!({
                "errors": [{"status": 409, "code": "fare_expired", "title": "The fare has expired."}],
                "meta": {"pickup": "secret street"}
            })))
            .mount(&server)
            .await;
        let err = uber(&server, None)
            .request("liz", &home(), &jkia(), &quote())
            .await
            .unwrap_err();
        let text = format!("{err:#}");
        assert!(text.contains("fare_expired"), "{text}");
        assert!(!text.contains("secret street"), "{text}");
    }

    #[tokio::test]
    async fn a_member_without_uber_makes_no_call() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        assert!(uber(&server, None)
            .quote("jerry", &home(), &jkia())
            .await
            .is_err());
    }

    #[test]
    fn unknown_statuses_are_kept_not_guessed() {
        assert_eq!(parse_status("arriving"), RideStatus::Arriving);
        assert_eq!(
            parse_status("driver_redispatched"),
            RideStatus::Other("driver_redispatched".to_string())
        );
    }
}
