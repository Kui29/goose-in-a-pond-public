//! Place -> coordinates. Split as a privacy boundary: [`NetworkPlaceLookup`] tells a third
//! party the household's IP, so it must be passed in on purpose, never folded into lookups.

use anyhow::Result;
use async_trait::async_trait;

/// A place, fixed to a point on the earth.
#[derive(Debug, Clone, PartialEq)]
pub struct PlaceFix {
    /// Readable name, e.g. `Kisumu, Kenya`.
    pub name: String,
    pub latitude: f64,
    pub longitude: f64,
    /// The zone the source believes this point is in, when it says.
    pub timezone: Option<String>,
}

/// One of the places a name could mean.
#[derive(Debug, Clone, PartialEq)]
pub struct PlaceCandidate {
    pub fix: PlaceFix,
    /// ISO 3166-1 alpha-2 (`KE`), when the source says.
    pub country_code: Option<String>,
}

/// Name → coordinates. Reveals the query, not the asker.
#[async_trait]
pub trait PlaceLookup: Send + Sync {
    /// The source's best match anywhere in the world.
    async fn by_name(&self, query: &str) -> Result<PlaceFix>;

    /// Up to `limit` places the name could mean, best first; only in `country_code` when given.
    /// Empty when nothing matches.
    async fn candidates(
        &self,
        query: &str,
        country_code: Option<&str>,
        limit: usize,
    ) -> Result<Vec<PlaceCandidate>>;
}

/// This connection → coordinates. Reveals the asker, so never wired by default.
#[async_trait]
pub trait NetworkPlaceLookup: Send + Sync {
    async fn by_network(&self) -> Result<PlaceFix>;
}
