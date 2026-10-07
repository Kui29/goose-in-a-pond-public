//! Place name → coordinates via the free, keyless Open-Meteo Geocoding API.

use anyhow::{Context, Result};
use std::time::Duration;

// ── Response shape ───────────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
struct GeoResponse {
    results: Option<Vec<GeoResult>>,
}

#[derive(serde::Deserialize)]
struct GeoResult {
    name: String,
    latitude: f64,
    longitude: f64,
    country: Option<String>,
    /// ISO 3166-1 alpha-2, e.g. `KE`.
    country_code: Option<String>,
    /// Open-Meteo's zone for this point: a second opinion on a stale system timezone.
    timezone: Option<String>,
}

// ── Public types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct GeoLocation {
    pub name: String,
    pub latitude: f64,
    pub longitude: f64,
    pub country: String,
    /// ISO 3166-1 alpha-2, e.g. `KE`, when the geocoder says.
    pub country_code: Option<String>,
    /// The zone the geocoder believes this point is in, when it says.
    pub timezone: Option<String>,
}

// ── Geocoder ─────────────────────────────────────────────────────────────────

pub struct Geocoder {
    client: reqwest::Client,
    base_url: String,
}

impl Geocoder {
    /// Create with the real Open-Meteo geocoding endpoint.
    pub fn new(client: reqwest::Client) -> Self {
        Self {
            client,
            base_url: "https://geocoding-api.open-meteo.com".to_string(),
        }
    }

    /// Create pointing at a custom base URL (tests with wiremock).
    pub fn with_base_url(client: reqwest::Client, base_url: impl Into<String>) -> Self {
        Self {
            client,
            base_url: base_url.into(),
        }
    }

    /// Resolve a place name (e.g. "Kisumu") to coordinates: the geocoder's best match anywhere.
    pub async fn geocode(&self, query: &str) -> Result<GeoLocation> {
        self.search(query, None, 1)
            .await?
            .into_iter()
            .next()
            .context(format!("no location found for '{query}'"))
    }

    /// Up to `count` places the name could mean, in the geocoder's order; only in
    /// `country_code` (ISO 3166-1 alpha-2) when given. Empty when nothing matches.
    pub async fn search(
        &self,
        query: &str,
        country_code: Option<&str>,
        count: usize,
    ) -> Result<Vec<GeoLocation>> {
        let mut url = format!(
            "{}/v1/search?name={}&count={count}&language=en",
            self.base_url,
            urlencoding::encode(query),
        );
        if let Some(code) = country_code {
            url.push_str(&format!("&countryCode={}", urlencoding::encode(code)));
        }

        tracing::debug!("geocoding: {url}");

        let resp = crate::traced_send(self.client.get(&url).timeout(Duration::from_secs(5)), &url)
            .await
            .context("geocoding request failed")?
            .error_for_status()
            .context("geocoding API returned error status")?;

        let geo: GeoResponse = resp
            .json()
            .await
            .context("failed to parse geocoding response")?;

        Ok(geo
            .results
            .unwrap_or_default()
            .into_iter()
            .map(located)
            .collect())
    }
}

fn located(result: GeoResult) -> GeoLocation {
    let name = match &result.country {
        Some(country) => format!("{}, {}", result.name, country),
        None => result.name.clone(),
    };
    GeoLocation {
        name,
        latitude: result.latitude,
        longitude: result.longitude,
        country: result.country.unwrap_or_default(),
        country_code: result.country_code,
        timezone: result.timezone,
    }
}

// ── The port ─────────────────────────────────────────────────────────────────

/// This geocoder as the core's [`PlaceLookup`].
#[async_trait::async_trait]
impl pond_core::user_data::ports::place_lookup::PlaceLookup for Geocoder {
    async fn by_name(
        &self,
        query: &str,
    ) -> Result<pond_core::user_data::ports::place_lookup::PlaceFix> {
        Ok(fix(self.geocode(query).await?))
    }

    async fn candidates(
        &self,
        query: &str,
        country_code: Option<&str>,
        limit: usize,
    ) -> Result<Vec<pond_core::user_data::ports::place_lookup::PlaceCandidate>> {
        Ok(self
            .search(query, country_code, limit)
            .await?
            .into_iter()
            .map(
                |g| pond_core::user_data::ports::place_lookup::PlaceCandidate {
                    country_code: g.country_code.clone(),
                    fix: fix(g),
                },
            )
            .collect())
    }
}

fn fix(g: GeoLocation) -> pond_core::user_data::ports::place_lookup::PlaceFix {
    pond_core::user_data::ports::place_lookup::PlaceFix {
        name: g.name,
        latitude: g.latitude,
        longitude: g.longitude,
        timezone: g.timezone,
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn mock_geocoding_response() -> serde_json::Value {
        serde_json::json!({
            "results": [{
                "name": "Kisumu",
                "latitude": -0.1022,
                "longitude": 34.7617,
                "country": "Kenya",
                "admin1": "Kisumu County"
            }]
        })
    }

    #[tokio::test]
    async fn geocodes_city_name() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(mock_geocoding_response()))
            .mount(&server)
            .await;

        let geocoder = Geocoder::with_base_url(reqwest::Client::new(), server.uri());
        let loc = geocoder.geocode("Kisumu").await.unwrap();

        assert_eq!(loc.name, "Kisumu, Kenya");
        assert!((loc.latitude - (-0.1022)).abs() < 0.001);
        assert!((loc.longitude - 34.7617).abs() < 0.001);
        assert_eq!(loc.country, "Kenya");
    }

    #[tokio::test]
    async fn search_asks_for_several_matches_in_one_country_and_keeps_their_codes() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/search"))
            .and(query_param("name", "Westlands"))
            .and(query_param("count", "10"))
            .and(query_param("countryCode", "KE"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "results": [{
                    "name": "Westlands", "latitude": -1.2676, "longitude": 36.8108,
                    "country": "Kenya", "country_code": "KE", "timezone": "Africa/Nairobi"
                }]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let geocoder = Geocoder::with_base_url(reqwest::Client::new(), server.uri());
        let found = geocoder.search("Westlands", Some("KE"), 10).await.unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "Westlands, Kenya");
        assert_eq!(found[0].country_code.as_deref(), Some("KE"));
    }

    #[tokio::test]
    async fn search_without_a_country_sends_none_and_nothing_found_is_empty() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&server)
            .await;

        let geocoder = Geocoder::with_base_url(reqwest::Client::new(), server.uri());
        assert!(geocoder.search("JKIA", None, 10).await.unwrap().is_empty());
        let asked = server.received_requests().await.unwrap();
        assert!(
            !asked[0].url.as_str().contains("countryCode"),
            "{}",
            asked[0].url
        );
    }

    #[tokio::test]
    async fn returns_error_for_empty_results() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/search"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "results": [] })),
            )
            .mount(&server)
            .await;

        let geocoder = Geocoder::with_base_url(reqwest::Client::new(), server.uri());
        assert!(geocoder.geocode("xyznonexistent").await.is_err());
    }

    #[tokio::test]
    async fn returns_error_for_null_results() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&server)
            .await;

        let geocoder = Geocoder::with_base_url(reqwest::Client::new(), server.uri());
        assert!(geocoder.geocode("xyznonexistent").await.is_err());
    }
}
