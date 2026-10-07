//! Travel MCP server: directions and ride-app links. The user opens each link and confirms in
//! the app; nothing here books, pays or tracks.

use pond_core::user_data::ports::place_lookup::{PlaceFix, PlaceLookup};
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, Content, ErrorData, Implementation, InitializeResult, ProtocolVersion,
        ServerCapabilities, ServerInfo,
    },
    service::RequestContext,
    tool, tool_handler, tool_router, RoleServer, ServerHandler,
};
use schemars::JsonSchema;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;

pub use pond_core::mcp::domain::tool_group::TRAVEL_EXTENSION;

// ── Parameter structs ─────────────────────────────────────────────────────────

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct DirectionsParams {
    pub destination: Option<String>,
    /// Omit for the phone's current location.
    pub origin: Option<String>,
    /// driving (default), walking, transit or cycling.
    pub mode: Option<String>,
    /// Catch-all for unexpected fields the model might send.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct RideParams {
    pub destination: Option<String>,
    /// Omit for the phone's current location.
    pub pickup: Option<String>,
    /// uber (default) or bolt.
    pub app: Option<String>,
    /// Catch-all for unexpected fields the model might send.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: HashMap<String, serde_json::Value>,
}

// ── Link building ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TravelMode {
    Driving,
    Walking,
    Transit,
    Cycling,
}

impl TravelMode {
    /// Unknown or missing words mean driving, the mode a ride-hailing household asks for most.
    pub fn parse(raw: Option<&str>) -> Self {
        match raw.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
            Some("walking" | "walk" | "foot") => Self::Walking,
            Some("transit" | "bus" | "matatu" | "train" | "public transport") => Self::Transit,
            Some("cycling" | "bicycling" | "bike" | "bicycle") => Self::Cycling,
            _ => Self::Driving,
        }
    }

    fn google(self) -> &'static str {
        match self {
            Self::Driving => "driving",
            Self::Walking => "walking",
            Self::Transit => "transit",
            Self::Cycling => "bicycling",
        }
    }

    /// Apple Maps has no cycling flag; `None` lets the app choose.
    fn apple(self) -> Option<&'static str> {
        match self {
            Self::Driving => Some("d"),
            Self::Walking => Some("w"),
            Self::Transit => Some("r"),
            Self::Cycling => None,
        }
    }
}

fn enc(s: &str) -> String {
    urlencoding::encode(s).into_owned()
}

/// Google Maps directions URL; without an origin the app starts from the phone's location.
pub fn google_maps_url(destination: &str, origin: Option<&str>, mode: TravelMode) -> String {
    let mut url = format!(
        "https://www.google.com/maps/dir/?api=1&destination={}&travelmode={}",
        enc(destination),
        mode.google()
    );
    if let Some(origin) = origin {
        url.push_str(&format!("&origin={}", enc(origin)));
    }
    url
}

/// Apple Maps directions URL; without `saddr` the app starts from the phone's location.
pub fn apple_maps_url(destination: &str, origin: Option<&str>, mode: TravelMode) -> String {
    let mut url = format!("https://maps.apple.com/?daddr={}", enc(destination));
    if let Some(origin) = origin {
        url.push_str(&format!("&saddr={}", enc(origin)));
    }
    if let Some(flag) = mode.apple() {
        url.push_str(&format!("&dirflg={flag}"));
    }
    url
}

/// A ride endpoint: a resolved place, or the phone's own location.
#[derive(Debug, Clone, PartialEq)]
pub enum RidePoint {
    MyLocation,
    Place {
        /// What the user said, shown in Uber as the address line.
        asked: String,
        fix: PlaceFix,
    },
}

/// Uber's documented universal link (`m.uber.com/ul`): opens the app, or the mobile site, on the
/// confirm screen. Uber requires coordinates for a drop-off, so `None` leaves it for the user.
pub fn uber_url(pickup: &RidePoint, dropoff: Option<&RidePoint>) -> String {
    let mut url = String::from("https://m.uber.com/ul/?action=setPickup");
    push_uber_point(&mut url, "pickup", pickup);
    if let Some(point) = dropoff {
        push_uber_point(&mut url, "dropoff", point);
    }
    url
}

fn push_uber_point(url: &mut String, role: &str, point: &RidePoint) {
    match point {
        RidePoint::MyLocation => url.push_str(&format!("&{role}=my_location")),
        RidePoint::Place { asked, fix } => url.push_str(&format!(
            "&{role}[latitude]={:.6}&{role}[longitude]={:.6}&{role}[nickname]={}\
             &{role}[formatted_address]={}",
            fix.latitude,
            fix.longitude,
            enc(&fix.name),
            enc(asked),
        )),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RideApp {
    Uber,
    Bolt,
}

impl RideApp {
    pub fn parse(raw: Option<&str>) -> Self {
        match raw.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
            Some(s) if s.contains("bolt") || s.contains("taxify") => Self::Bolt,
            _ => Self::Uber,
        }
    }
}

// ── MCP server ─────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct TravelMcpServer {
    places: Option<Arc<dyn PlaceLookup>>,
    #[allow(dead_code)] // accessed by rmcp's generated tool_handler code
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl TravelMcpServer {
    /// Every tool this server exposes, without constructing it (`tool_router()` is private).
    pub(crate) fn tool_defs() -> Vec<rmcp::model::Tool> {
        Self::tool_router().list_all()
    }

    pub fn new(places: Option<Arc<dyn PlaceLookup>>) -> Self {
        Self {
            places,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(description = "\
Directions to a place as Google Maps and Apple Maps links the user opens on \
their phone. Omit origin for their current location. Never invent routes or times.")]
    async fn get_directions_link(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<DirectionsParams>,
    ) -> Result<CallToolResult, ErrorData> {
        crate::set_current_tool("get_directions_link");
        Ok(directions_result(&params.0))
    }

    #[tool(description = "\
Link that opens Uber (default) or Bolt with a ride filled in; the user confirms \
and pays in the app. Omit pickup for their current location. Never claim a ride is booked.")]
    async fn get_ride_link(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<RideParams>,
    ) -> Result<CallToolResult, ErrorData> {
        crate::set_current_tool("get_ride_link");
        Ok(self.ride_result(&params.0).await)
    }
}

impl TravelMcpServer {
    /// The body of `get_ride_link`, apart from the rmcp wrapper so tests can call it.
    pub async fn ride_result(&self, params: &RideParams) -> CallToolResult {
        let destination = text_param(&params.destination, &params.extra, DESTINATION_KEYS);
        let pickup = text_param(&params.pickup, &params.extra, PICKUP_KEYS);
        let app = RideApp::parse(
            params
                .app
                .as_deref()
                .or_else(|| extra_str(&params.extra, &["provider", "service"])),
        );

        if app == RideApp::Bolt {
            return CallToolResult::error(vec![Content::text(format!(
                "Bolt publishes no link that fills in a trip, so none was made. Nothing has been \
                 booked.{}",
                destination
                    .map(|d| format!(" Destination asked for: {d}."))
                    .unwrap_or_default()
            ))]);
        }

        let mut notes = Vec::new();

        let pickup_point = match &pickup {
            None => RidePoint::MyLocation,
            Some(asked) => match self.resolve(asked).await {
                Ok(fix) => RidePoint::Place {
                    asked: asked.clone(),
                    fix,
                },
                Err(why) => {
                    notes.push(format!(
                        "Pickup is the phone's current location: {why} for \"{asked}\"."
                    ));
                    RidePoint::MyLocation
                }
            },
        };

        let dropoff_point = match &destination {
            None => {
                notes.push("No destination was given, so the drop-off is empty.".to_string());
                None
            }
            Some(asked) => match self.resolve(asked).await {
                Ok(fix) => Some(RidePoint::Place {
                    asked: asked.clone(),
                    fix,
                }),
                Err(why) => {
                    notes.push(format!("The drop-off is empty: {why} for \"{asked}\"."));
                    None
                }
            },
        };

        let mut lines = vec![format!(
            "Uber link: {}",
            uber_url(&pickup_point, dropoff_point.as_ref())
        )];
        lines.push(format!("Pickup: {}", describe(&pickup_point)));
        if let Some(point) = &dropoff_point {
            lines.push(format!("Drop-off: {}", describe(point)));
        }
        lines.extend(notes);
        lines.push(
            "Nothing has been booked. The ride is requested only when the user confirms it in \
             Uber, which also shows the fare."
                .to_string(),
        );
        CallToolResult::success(vec![Content::text(lines.join("\n"))])
    }

    async fn resolve(&self, query: &str) -> Result<PlaceFix, String> {
        let Some(places) = &self.places else {
            return Err("this pond has no place lookup".to_string());
        };
        places.by_name(query).await.map_err(|e| {
            tracing::debug!(error = %e, "travel: place lookup failed");
            // The geocoder's empty-result error; anything else (an offline refusal) is a failure.
            if format!("{e:#}").contains("no location found") {
                "no place matched".to_string()
            } else {
                "the place lookup failed".to_string()
            }
        })
    }
}

fn describe(point: &RidePoint) -> String {
    match point {
        RidePoint::MyLocation => "the phone's current location".to_string(),
        // The lookup matches towns and landmarks, not addresses, so the match is always shown.
        RidePoint::Place { asked, fix } => format!("{} (matched from \"{asked}\")", fix.name),
    }
}

/// The body of `get_directions_link`. Pure: the maps apps resolve free text themselves.
pub fn directions_result(params: &DirectionsParams) -> CallToolResult {
    let Some(destination) = text_param(&params.destination, &params.extra, DESTINATION_KEYS) else {
        return CallToolResult::error(vec![Content::text(
            "No destination was given, so no directions link was made.",
        )]);
    };
    let origin = text_param(&params.origin, &params.extra, ORIGIN_KEYS);
    let mode = TravelMode::parse(
        params
            .mode
            .as_deref()
            .or_else(|| extra_str(&params.extra, &["travelmode", "travel_mode", "by"])),
    );
    let from = origin
        .as_deref()
        .map(|o| format!("from {o}"))
        .unwrap_or_else(|| "from the phone's current location".to_string());
    CallToolResult::success(vec![Content::text(format!(
        "Directions to {destination}, {from}, {}.\nGoogle Maps: {}\nApple Maps: {}",
        mode.google(),
        google_maps_url(&destination, origin.as_deref(), mode),
        apple_maps_url(&destination, origin.as_deref(), mode),
    ))])
}

// ── Param resolution ──────────────────────────────────────────────────────────

const DESTINATION_KEYS: &[&str] = &["destination", "to", "dropoff", "place", "where", "address"];
const ORIGIN_KEYS: &[&str] = &["origin", "from", "start"];
const PICKUP_KEYS: &[&str] = &["pickup", "from", "origin", "start"];

/// The named param, else the first non-blank string among `keys` in the extras.
fn text_param(
    direct: &Option<String>,
    extra: &HashMap<String, serde_json::Value>,
    keys: &[&str],
) -> Option<String> {
    direct
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| extra_str(extra, keys))
        .map(str::to_string)
}

fn extra_str<'a>(extra: &'a HashMap<String, serde_json::Value>, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .filter_map(|k| extra.get(*k).and_then(|v| v.as_str()))
        .map(str::trim)
        .find(|s| !s.is_empty())
}

#[tool_handler]
impl ServerHandler for TravelMcpServer {
    fn get_info(&self) -> ServerInfo {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_server_info(Implementation::new(
                TRAVEL_EXTENSION,
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "GIAP Travel MCP server — links the user opens on their phone.\n\n\
                 Tools:\n\
                 - get_directions_link: Google Maps and Apple Maps directions to a place.\n\
                 - get_ride_link: an Uber link with pickup and drop-off filled in. Bolt has no \
                 such link.\n\n\
                 Nothing here books, pays for or tracks a ride.",
            )
    }
}

// ── Static deps + spawn function for Goose builtin registry ──────────────

use std::sync::OnceLock;
use tokio::io::DuplexStream;

struct TravelDeps {
    places: Option<Arc<dyn PlaceLookup>>,
}

static TRAVEL_DEPS: OnceLock<TravelDeps> = OnceLock::new();

/// Install the place lookup. Call once at startup.
pub fn init_travel_deps(places: Option<Arc<dyn PlaceLookup>>) {
    let _ = TRAVEL_DEPS.set(TravelDeps { places });
}

/// Spawn function compatible with Goose's `SpawnServerFn` type.
pub fn spawn_travel_server(reader: DuplexStream, writer: DuplexStream) {
    // No panic: a panic in one spawn fn kills every builtin's startup.
    let Some(deps) = TRAVEL_DEPS.get() else {
        tracing::error!(
            "spawn_travel_server called before init_travel_deps — extension will not start"
        );
        return;
    };
    let server = TravelMcpServer::new(deps.places.clone());
    crate::serve_builtin(TRAVEL_EXTENSION, server, reader, writer);
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    struct StubPlaces;

    #[async_trait]
    impl PlaceLookup for StubPlaces {
        async fn by_name(&self, query: &str) -> anyhow::Result<PlaceFix> {
            match query {
                "JKIA" => Ok(PlaceFix {
                    name: "Jomo Kenyatta International Airport, Kenya".to_string(),
                    latitude: -1.319167,
                    longitude: 36.9275,
                    timezone: None,
                }),
                "Westlands" => Ok(PlaceFix {
                    name: "Westlands, Kenya".to_string(),
                    latitude: -1.2676,
                    longitude: 36.8108,
                    timezone: None,
                }),
                _ => anyhow::bail!("no location found for '{query}'"),
            }
        }
    }

    fn server() -> TravelMcpServer {
        TravelMcpServer::new(Some(Arc::new(StubPlaces)))
    }

    fn text(result: &CallToolResult) -> String {
        result
            .content
            .iter()
            .filter_map(|c| c.as_text().map(|t| t.text.clone()))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn ride(destination: Option<&str>, pickup: Option<&str>, app: Option<&str>) -> RideParams {
        RideParams {
            destination: destination.map(str::to_string),
            pickup: pickup.map(str::to_string),
            app: app.map(str::to_string),
            extra: HashMap::new(),
        }
    }

    #[test]
    fn google_link_encodes_the_destination_and_omits_a_missing_origin() {
        let url = google_maps_url("Sarit Centre, Nairobi", None, TravelMode::Driving);
        assert_eq!(
            url,
            "https://www.google.com/maps/dir/?api=1&destination=Sarit%20Centre%2C%20Nairobi\
             &travelmode=driving"
        );
    }

    #[test]
    fn google_link_carries_an_origin_and_the_mode() {
        let url = google_maps_url("Kisumu", Some("Nakuru"), TravelMode::Cycling);
        assert!(url.contains("&origin=Nakuru"), "{url}");
        assert!(url.contains("&travelmode=bicycling"), "{url}");
    }

    #[test]
    fn apple_link_uses_daddr_saddr_and_flags() {
        assert_eq!(
            apple_maps_url("Kisumu", Some("Nakuru"), TravelMode::Transit),
            "https://maps.apple.com/?daddr=Kisumu&saddr=Nakuru&dirflg=r"
        );
        assert_eq!(
            apple_maps_url("Kisumu", None, TravelMode::Cycling),
            "https://maps.apple.com/?daddr=Kisumu"
        );
    }

    #[test]
    fn mode_words_map_and_unknown_means_driving() {
        assert_eq!(TravelMode::parse(Some("Walk")), TravelMode::Walking);
        assert_eq!(TravelMode::parse(Some("matatu")), TravelMode::Transit);
        assert_eq!(TravelMode::parse(Some("bike")), TravelMode::Cycling);
        assert_eq!(TravelMode::parse(Some("hovercraft")), TravelMode::Driving);
        assert_eq!(TravelMode::parse(None), TravelMode::Driving);
    }

    #[test]
    fn uber_link_with_my_location_and_a_resolved_dropoff() {
        let dropoff = RidePoint::Place {
            asked: "JKIA".to_string(),
            fix: PlaceFix {
                name: "JKIA, Kenya".to_string(),
                latitude: -1.319167,
                longitude: 36.9275,
                timezone: None,
            },
        };
        assert_eq!(
            uber_url(&RidePoint::MyLocation, Some(&dropoff)),
            "https://m.uber.com/ul/?action=setPickup&pickup=my_location\
             &dropoff[latitude]=-1.319167&dropoff[longitude]=36.927500\
             &dropoff[nickname]=JKIA%2C%20Kenya&dropoff[formatted_address]=JKIA"
        );
    }

    #[test]
    fn uber_link_without_a_dropoff_sets_only_the_pickup() {
        assert_eq!(
            uber_url(&RidePoint::MyLocation, None),
            "https://m.uber.com/ul/?action=setPickup&pickup=my_location"
        );
    }

    #[test]
    fn ride_app_defaults_to_uber() {
        assert_eq!(RideApp::parse(None), RideApp::Uber);
        assert_eq!(RideApp::parse(Some("an Uber please")), RideApp::Uber);
        assert_eq!(RideApp::parse(Some("Bolt")), RideApp::Bolt);
    }

    #[tokio::test]
    async fn a_resolved_ride_names_the_match_and_books_nothing() {
        let out = text(&server().ride_result(&ride(Some("JKIA"), None, None)).await);
        assert!(out.contains("https://m.uber.com/ul/?action=setPickup&pickup=my_location"));
        assert!(out.contains("dropoff[latitude]=-1.319167"), "{out}");
        assert!(
            out.contains("Jomo Kenyatta International Airport, Kenya"),
            "{out}"
        );
        assert!(out.contains("Nothing has been booked"), "{out}");
    }

    #[tokio::test]
    async fn a_named_pickup_is_resolved_too() {
        let out = text(
            &server()
                .ride_result(&ride(Some("JKIA"), Some("Westlands"), None))
                .await,
        );
        assert!(out.contains("pickup[latitude]=-1.267600"), "{out}");
        assert!(!out.contains("pickup=my_location"), "{out}");
    }

    #[tokio::test]
    async fn an_unmatched_destination_leaves_the_dropoff_empty_and_says_so() {
        let result = server()
            .ride_result(&ride(Some("my aunt's place"), None, None))
            .await;
        let out = text(&result);
        assert_ne!(result.is_error, Some(true));
        assert!(!out.contains("dropoff["), "{out}");
        assert!(out.contains("The drop-off is empty"), "{out}");
    }

    #[tokio::test]
    async fn without_a_place_lookup_the_link_still_opens_uber() {
        let out = text(
            &TravelMcpServer::new(None)
                .ride_result(&ride(Some("JKIA"), None, None))
                .await,
        );
        assert!(out.contains("pickup=my_location"), "{out}");
        assert!(out.contains("this pond has no place lookup"), "{out}");
    }

    #[tokio::test]
    async fn bolt_is_an_error_that_makes_no_link() {
        let result = server()
            .ride_result(&ride(Some("JKIA"), None, Some("bolt")))
            .await;
        assert_eq!(result.is_error, Some(true));
        let out = text(&result);
        assert!(!out.contains("https://"), "{out}");
        assert!(out.contains("Nothing has been booked"), "{out}");
    }

    #[test]
    fn directions_read_the_destination_from_extras() {
        let mut extra = HashMap::new();
        extra.insert("to".to_string(), serde_json::json!("Kisumu"));
        let out = text(&directions_result(&DirectionsParams {
            extra,
            ..Default::default()
        }));
        assert!(out.contains("destination=Kisumu"), "{out}");
        assert!(out.contains("daddr=Kisumu"), "{out}");
    }

    #[test]
    fn directions_without_a_destination_are_an_error() {
        let result = directions_result(&DirectionsParams::default());
        assert_eq!(result.is_error, Some(true));
    }
}

#[cfg(test)]
mod result_wording_tests {
    //! The model acts on what a result names, so results state facts and never instruct it.

    fn tool_bodies() -> &'static str {
        let src = include_str!("travel.rs");
        let end = src.find("#[cfg(test)]").unwrap_or(src.len());
        &src[..end]
    }

    #[test]
    fn no_travel_result_tells_the_model_what_to_tell_the_user() {
        for phrase in ["Tell the user", "Inform the user", "suggest", "DO NOT"] {
            assert!(
                !tool_bodies().contains(phrase),
                "a travel result instructs the model ({phrase:?}); state the fact instead"
            );
        }
    }
}
