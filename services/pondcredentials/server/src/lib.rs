//! pondcredentials: hands out Apple Music developer tokens to ponds, so a household needs no Apple
//! developer key of its own. It holds one MusicKit key and answers one question. Optionally it
//! also relays Uber sign-ins, adding Jarida's Uber client secret (see [`uber`]).
//!
//! What it deliberately does not do: identify callers, keep an address, or hold a user's Music
//! User Token (that never leaves the household's own player). A token is not secret, since every
//! MusicKit web page ships one to the browser; the KEY is, and it never leaves this process.

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::body::Bytes;
use axum::extract::{ConnectInfo, DefaultBodyLimit, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use pond_apple_token::{SigningCredentials, APPLE_MAX_TTL};
use serde_json::json;

pub mod uber;

const DAY: Duration = Duration::from_secs(24 * 60 * 60);

// ── Configuration ────────────────────────────────────────────

pub struct Config {
    pub team_id: String,
    pub key_id: String,
    pub private_key: String,
    /// How long a token lives.
    pub ttl: Duration,
    /// How old the token being served may get before a fresh one is signed.
    pub refresh_after: Duration,
    pub rate_per_minute: u32,
    pub trust_proxy: bool,
    /// `None`: the Uber routes answer 503 and the service serves MusicKit tokens as before.
    pub uber: Option<uber::UberConfig>,
}

fn ten_chars(name: &str, value: &str) -> Result<(), String> {
    if value.len() == 10 && value.bytes().all(|b| b.is_ascii_alphanumeric()) {
        Ok(())
    } else {
        Err(format!(
            "{name} must be the 10 letters and digits Apple gave you."
        ))
    }
}

impl Config {
    /// `get` reads one setting, so tests need not touch the process environment. The key comes
    /// from a file, never from a variable: a variable shows up in `docker inspect` and in `ps`.
    pub fn from_env(
        get: impl Fn(&str) -> Option<String>,
        read_file: impl Fn(&str) -> Result<String, String>,
    ) -> Result<Config, String> {
        let need = |name: &str| {
            get(name)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .ok_or_else(|| format!("{name} is not set."))
        };
        let team_id = need("APPLE_TEAM_ID")?;
        let key_id = need("APPLE_KEY_ID")?;
        ten_chars("APPLE_TEAM_ID", &team_id)?;
        ten_chars("APPLE_KEY_ID", &key_id)?;

        let uber = uber::UberConfig::from_env(&get, &read_file)?;

        let path = need("APPLE_PRIVATE_KEY_FILE")?;
        let private_key =
            read_file(&path).map_err(|e| format!("cannot read APPLE_PRIVATE_KEY_FILE: {e}"))?;

        let number = |name: &str, default: u64| -> Result<u64, String> {
            match get(name)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
            {
                None => Ok(default),
                Some(v) => v
                    .parse::<u64>()
                    .map_err(|_| format!("{name} must be a whole number, got \"{v}\".")),
            }
        };

        let ttl_days = number("TOKEN_TTL_DAYS", 30)?;
        let ttl = DAY * ttl_days as u32;
        if ttl_days == 0 || ttl > APPLE_MAX_TTL {
            return Err("TOKEN_TTL_DAYS must be between 1 and 182: Apple refuses a token that lives longer.".into());
        }
        let refresh_hours = number("TOKEN_REFRESH_HOURS", 24)?;
        if refresh_hours == 0 {
            return Err("TOKEN_REFRESH_HOURS must be at least 1.".into());
        }
        let rate = number("RATE_LIMIT_PER_MINUTE", 20)?;
        if rate == 0 {
            return Err("RATE_LIMIT_PER_MINUTE must be at least 1.".into());
        }

        Ok(Config {
            team_id,
            key_id,
            private_key,
            ttl,
            refresh_after: Duration::from_secs(refresh_hours * 3600),
            rate_per_minute: rate as u32,
            trust_proxy: matches!(get("TRUST_PROXY").as_deref(), Some("1" | "true")),
            uber,
        })
    }
}

// ── Issuing ──────────────────────────────────────────────────

#[derive(Clone)]
pub struct Issued {
    pub token: String,
    pub expires_at: u64,
    pub issued_at: u64,
}

/// Signs a token, and reuses it until it is old enough that a household fetching it would rather
/// have a fresher one. One signature a day, however many ponds ask.
pub struct Issuer {
    credentials: SigningCredentials,
    ttl: Duration,
    refresh_after: Duration,
    cached: Mutex<Option<Issued>>,
}

impl Issuer {
    /// Signs once now, so a key that cannot sign stops the service starting instead of failing
    /// the first household to ask.
    pub fn new(config: &Config, now: u64) -> Result<Issuer, String> {
        let issuer = Issuer {
            credentials: SigningCredentials {
                team_id: config.team_id.clone(),
                key_id: config.key_id.clone(),
                private_key: config.private_key.clone(),
            },
            ttl: config.ttl,
            refresh_after: config.refresh_after,
            cached: Mutex::new(None),
        };
        issuer.token(now)?;
        Ok(issuer)
    }

    pub fn token(&self, now: u64) -> Result<Issued, String> {
        let mut cached = self.cached.lock().unwrap();
        if let Some(issued) = cached.as_ref() {
            if now >= issued.issued_at && now - issued.issued_at < self.refresh_after.as_secs() {
                return Ok(issued.clone());
            }
        }
        let (token, expires_at) = self.credentials.sign(now, self.ttl)?;
        let issued = Issued {
            token,
            expires_at,
            issued_at: now,
        };
        *cached = Some(issued.clone());
        Ok(issued)
    }
}

// ── Rate limiting ────────────────────────────────────────────

/// Who is asking, as far as the limit is concerned. An IPv6 client owns a whole /64, so it is
/// keyed by that: otherwise rotating addresses inside one block would dodge any limit.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Client {
    V4([u8; 4]),
    V6Block([u8; 8]),
}

impl Client {
    pub fn of(ip: IpAddr) -> Client {
        match ip {
            IpAddr::V4(v4) => Client::V4(v4.octets()),
            IpAddr::V6(v6) => {
                let mut block = [0u8; 8];
                block.copy_from_slice(&v6.octets()[..8]);
                Client::V6Block(block)
            }
        }
    }
}

/// A sliding window per client, held in memory only and never written or logged.
pub struct RateLimiter {
    limit: u32,
    window: Duration,
    max_tracked: usize,
    hits: Mutex<HashMap<Client, VecDeque<Instant>>>,
}

impl RateLimiter {
    pub fn new(limit: u32, window: Duration, max_tracked: usize) -> RateLimiter {
        RateLimiter {
            limit,
            window,
            max_tracked,
            hits: Mutex::new(HashMap::new()),
        }
    }

    /// `Ok` and the hit is counted, or `Err(seconds until the oldest hit leaves the window)`.
    pub fn check(&self, client: &Client, now: Instant) -> Result<(), u64> {
        let mut hits = self.hits.lock().unwrap();

        // Bounded memory: a flood of distinct addresses must not grow this without limit.
        if hits.len() >= self.max_tracked && !hits.contains_key(client) {
            hits.retain(|_, q| {
                q.back()
                    .is_some_and(|last| now.duration_since(*last) < self.window)
            });
            if hits.len() >= self.max_tracked {
                return Err(self.window.as_secs().max(1));
            }
        }

        let queue = hits.entry(client.clone()).or_default();
        while queue
            .front()
            .is_some_and(|first| now.duration_since(*first) >= self.window)
        {
            queue.pop_front();
        }
        if queue.len() as u32 >= self.limit {
            let oldest = *queue.front().expect("a full window has a first hit");
            let wait = self.window.saturating_sub(now.duration_since(oldest));
            return Err(wait.as_secs().max(1));
        }
        queue.push_back(now);
        Ok(())
    }
}

/// The client to rate-limit. Behind Caddy the peer is always Caddy, so the address comes from the
/// LAST `X-Forwarded-For` entry, the one our own proxy appended; earlier ones are the caller's to
/// forge. Trusted only when told to be, since anywhere else the header is just a claim.
pub fn client_of(headers: &HeaderMap, peer: SocketAddr, trust_proxy: bool) -> Client {
    if trust_proxy {
        let forwarded = headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.rsplit(',').next())
            .and_then(|v| v.trim().parse::<IpAddr>().ok());
        if let Some(ip) = forwarded {
            return Client::of(ip);
        }
    }
    Client::of(peer.ip())
}

// ── The service ──────────────────────────────────────────────

#[derive(Default)]
pub struct Metrics {
    pub issued: AtomicU64,
    pub limited: AtomicU64,
    pub uber_relayed: AtomicU64,
}

pub struct AppState {
    pub issuer: Issuer,
    pub limiter: RateLimiter,
    pub metrics: Metrics,
    pub trust_proxy: bool,
    pub uber: Option<uber::UberRelay>,
    pub started: Instant,
    pub clock: Box<dyn Fn() -> u64 + Send + Sync>,
}

pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl AppState {
    pub fn new(config: &Config) -> Result<AppState, String> {
        let now = unix_now();
        let issuer = Issuer::new(config, now)?;
        let uber = config.uber.as_ref().map(uber::UberRelay::new).transpose()?;
        Ok(AppState {
            issuer,
            limiter: RateLimiter::new(config.rate_per_minute, Duration::from_secs(60), 10_000),
            metrics: Metrics::default(),
            trust_proxy: config.trust_proxy,
            uber,
            started: Instant::now(),
            clock: Box::new(unix_now),
        })
    }
}

fn plain(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

/// `Some(429)` when this client is over its allowance; every route shares one allowance.
fn over_limit(state: &AppState, headers: &HeaderMap, peer: SocketAddr) -> Option<Response> {
    let client = client_of(headers, peer, state.trust_proxy);
    let retry_after = state.limiter.check(&client, Instant::now()).err()?;
    state.metrics.limited.fetch_add(1, Ordering::Relaxed);
    let mut resp = plain(
        StatusCode::TOO_MANY_REQUESTS,
        "Too many requests. Try again later.",
    );
    if let Ok(v) = HeaderValue::from_str(&retry_after.to_string()) {
        resp.headers_mut().insert(header::RETRY_AFTER, v);
    }
    Some(resp)
}

/// Nothing in between should keep a copy of a token.
fn no_store(mut resp: Response) -> Response {
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

async fn developer_token(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    // Never used, but reading it is what makes DefaultBodyLimit apply: a handler that ignores
    // its body would accept an upload of any size.
    _body: Bytes,
) -> Response {
    if let Some(limited) = over_limit(&state, &headers, peer) {
        return limited;
    }

    match state.issuer.token((state.clock)()) {
        Ok(issued) => {
            state.metrics.issued.fetch_add(1, Ordering::Relaxed);
            no_store(
                Json(json!({
                    "token": issued.token,
                    "expires_at": issued.expires_at,
                }))
                .into_response(),
            )
        }
        Err(error) => {
            // The reason can name the key; the caller gets nothing it could use.
            tracing::error!(%error, "could not sign a developer token");
            plain(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Could not issue a token.",
            )
        }
    }
}

fn uber_not_configured() -> Response {
    plain(
        StatusCode::SERVICE_UNAVAILABLE,
        "This service does not relay Uber sign-ins.",
    )
}

fn relayed(state: &AppState, outcome: uber::Relayed) -> Response {
    match outcome {
        uber::Relayed::Tokens(tokens) => {
            state.metrics.uber_relayed.fetch_add(1, Ordering::Relaxed);
            no_store(Json(tokens).into_response())
        }
        uber::Relayed::Refused(code) => no_store(
            (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "Uber refused the sign-in.", "uber_error": code })),
            )
                .into_response(),
        ),
        uber::Relayed::BadRequest(why) => plain(StatusCode::BAD_REQUEST, why),
        uber::Relayed::Unavailable => plain(StatusCode::BAD_GATEWAY, "Uber could not be reached."),
    }
}

async fn uber_client(State(state): State<Arc<AppState>>) -> Response {
    match &state.uber {
        Some(relay) => Json(json!({ "client_id": relay.client_id() })).into_response(),
        None => uber_not_configured(),
    }
}

async fn uber_token(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<uber::CodeExchange>,
) -> Response {
    if let Some(limited) = over_limit(&state, &headers, peer) {
        return limited;
    }
    let Some(relay) = &state.uber else {
        return uber_not_configured();
    };
    let outcome = relay.exchange_code(&request).await;
    relayed(&state, outcome)
}

async fn uber_refresh(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<uber::Refresh>,
) -> Response {
    if let Some(limited) = over_limit(&state, &headers, peer) {
        return limited;
    }
    let Some(relay) = &state.uber else {
        return uber_not_configured();
    };
    let outcome = relay.refresh(&request).await;
    relayed(&state, outcome)
}

async fn healthz(State(state): State<Arc<AppState>>) -> Response {
    Json(json!({
        "ok": true,
        "issued": state.metrics.issued.load(Ordering::Relaxed),
        "limited": state.metrics.limited.load(Ordering::Relaxed),
        "uber_relayed": state.metrics.uber_relayed.load(Ordering::Relaxed),
        "uptime_s": state.started.elapsed().as_secs(),
    }))
    .into_response()
}

async fn not_found() -> Response {
    plain(StatusCode::NOT_FOUND, "Not found.")
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/v1/musickit/developer-token", post(developer_token))
        .route("/v1/uber/client", get(uber_client))
        .route("/v1/uber/token", post(uber_token))
        .route("/v1/uber/refresh", post(uber_refresh))
        .route("/healthz", get(healthz))
        .fallback(not_found)
        // The biggest body is an Uber refresh token in a little JSON; refuse anything of size.
        .layer(DefaultBodyLimit::max(1024))
        .layer(axum::middleware::map_response(
            |mut resp: Response| async move {
                resp.headers_mut().insert(
                    "x-content-type-options",
                    HeaderValue::from_static("nosniff"),
                );
                resp
            },
        ))
        .with_state(state)
}
