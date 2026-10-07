//! Distances, and which of the places sharing a name a household means: the one nearest home.
//! A geocoder ranks matches worldwide, so its first "Westlands" can be in Jamaica.

use anyhow::Result;

use crate::user_data::ports::place_lookup::{PlaceCandidate, PlaceFix, PlaceLookup};
use crate::user_data::services::location::Location;

/// A match farther than this from home is not the place the household means.
pub const NEAR_HOME_KM: f64 = 50.0;

/// How many matches to compare: enough that a suburb outranked by bigger namesakes still shows.
pub const CANDIDATES: usize = 10;

/// Mean earth radius, the usual figure for haversine distances.
const EARTH_RADIUS_KM: f64 = 6371.0;

/// Great-circle distance between two `(latitude, longitude)` points, in km.
pub fn distance_km(a: (f64, f64), b: (f64, f64)) -> f64 {
    let (lat1, lat2) = (a.0.to_radians(), b.0.to_radians());
    let d_lat = (b.0 - a.0).to_radians();
    let d_lon = (b.1 - a.1).to_radians();
    let h = (d_lat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (d_lon / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_KM * h.sqrt().min(1.0).asin()
}

/// The candidate nearest `home`, with its distance in km.
pub fn nearest(candidates: &[PlaceCandidate], home: (f64, f64)) -> Option<(&PlaceCandidate, f64)> {
    candidates
        .iter()
        .map(|c| (c, distance_km(home, (c.fix.latitude, c.fix.longitude))))
        .min_by(|a, b| a.1.total_cmp(&b.1))
}

/// The country the household is in, from its own place name matched near its coordinates.
/// `Ok(None)` when the pond has no coordinates or name, or the name matches nothing near them.
pub async fn home_country(lookup: &dyn PlaceLookup, home: &Location) -> Result<Option<String>> {
    // A stored name may carry its country after a comma, which the geocoder does not match.
    let name = home.name.split(',').next().unwrap_or_default().trim();
    if !home.has_coordinates() || name.is_empty() {
        return Ok(None);
    }
    let candidates = lookup.candidates(name, None, CANDIDATES).await?;
    Ok(nearest(&candidates, (home.latitude, home.longitude))
        .filter(|(_, km)| *km <= NEAR_HOME_KM)
        .and_then(|(c, _)| c.country_code.clone()))
}

/// The place `query` names within [`NEAR_HOME_KM`] of `home`, searching `country_code` only when
/// known; `Ok(None)` when no match is that close.
pub async fn near_home(
    lookup: &dyn PlaceLookup,
    query: &str,
    home: (f64, f64),
    country_code: Option<&str>,
) -> Result<Option<PlaceFix>> {
    let candidates = lookup.candidates(query, country_code, CANDIDATES).await?;
    Ok(nearest(&candidates, home)
        .filter(|(_, km)| *km <= NEAR_HOME_KM)
        .map(|(c, _)| c.fix.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_data::services::location::Origin;
    use async_trait::async_trait;
    use std::sync::Mutex;

    const NAIROBI: (f64, f64) = (-1.286, 36.817);

    fn candidate(name: &str, lat: f64, lon: f64, country: &str) -> PlaceCandidate {
        PlaceCandidate {
            fix: PlaceFix {
                name: name.to_string(),
                latitude: lat,
                longitude: lon,
                timezone: None,
            },
            country_code: Some(country.to_string()),
        }
    }

    /// Answers like the real geocoder: namesakes worldwide first, the household's one later.
    #[derive(Default)]
    struct Worldwide {
        asked: Mutex<Vec<(String, Option<String>)>>,
    }

    #[async_trait]
    impl PlaceLookup for Worldwide {
        async fn by_name(&self, query: &str) -> Result<PlaceFix> {
            anyhow::bail!("by_name is not what a biased lookup asks for ({query})")
        }
        async fn candidates(
            &self,
            query: &str,
            country_code: Option<&str>,
            _limit: usize,
        ) -> Result<Vec<PlaceCandidate>> {
            self.asked
                .lock()
                .unwrap()
                .push((query.to_string(), country_code.map(str::to_string)));
            let all = match query {
                "Westlands" => vec![
                    candidate("Westlands, Jamaica", 18.03, -76.79, "JM"),
                    candidate("Westlands, Kenya", -1.2676, 36.8108, "KE"),
                ],
                "Nairobi" => vec![candidate("Nairobi, Kenya", -1.2833, 36.8167, "KE")],
                "Kisumu" => vec![candidate("Kisumu, Kenya", -0.1022, 34.7617, "KE")],
                _ => Vec::new(),
            };
            Ok(all
                .into_iter()
                .filter(|c| country_code.is_none_or(|cc| c.country_code.as_deref() == Some(cc)))
                .collect())
        }
    }

    fn home(name: &str) -> Location {
        Location {
            name: name.to_string(),
            latitude: NAIROBI.0,
            longitude: NAIROBI.1,
            timezone: "Africa/Nairobi".to_string(),
            origin: Origin::Configured,
        }
    }

    #[test]
    fn distances_match_known_figures() {
        // Nairobi to Mombasa is about 440 km as the crow flies.
        let km = distance_km(NAIROBI, (-4.0435, 39.6682));
        assert!((km - 440.0).abs() < 10.0, "{km}");
        assert_eq!(distance_km(NAIROBI, NAIROBI), 0.0);
    }

    #[tokio::test]
    async fn the_namesake_nearest_home_wins_over_the_first_match() {
        let geo = Worldwide::default();
        let fix = near_home(&geo, "Westlands", NAIROBI, None)
            .await
            .unwrap()
            .expect("Westlands, Kenya is 2 km from home");
        assert_eq!(fix.name, "Westlands, Kenya");
    }

    #[tokio::test]
    async fn a_place_only_far_from_home_is_no_match() {
        let geo = Worldwide::default();
        assert_eq!(
            near_home(&geo, "Kisumu", NAIROBI, None).await.unwrap(),
            None
        );
        assert_eq!(
            near_home(&geo, "nowhere at all", NAIROBI, None)
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn the_country_comes_from_the_homes_own_name_and_is_searched() {
        let geo = Worldwide::default();
        let country = home_country(&geo, &home("Nairobi, Kenya")).await.unwrap();
        assert_eq!(country.as_deref(), Some("KE"));
        near_home(&geo, "Westlands", NAIROBI, country.as_deref())
            .await
            .unwrap();
        let asked = geo.asked.lock().unwrap().clone();
        assert_eq!(asked[0], ("Nairobi".to_string(), None));
        assert_eq!(asked[1], ("Westlands".to_string(), Some("KE".to_string())));
    }

    #[tokio::test]
    async fn no_country_without_coordinates_or_a_name_that_matches_near_them() {
        let geo = Worldwide::default();
        let mut unplaced = home("Nairobi");
        unplaced.latitude = 0.0;
        unplaced.longitude = 0.0;
        assert_eq!(home_country(&geo, &unplaced).await.unwrap(), None);
        assert_eq!(home_country(&geo, &home("Kisumu")).await.unwrap(), None);
        assert_eq!(home_country(&geo, &home("")).await.unwrap(), None);
    }
}
