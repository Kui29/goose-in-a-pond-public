# Authentication and network security posture

How `pond-server` authenticates clients and what it exposes on the network.
Covers issues #4, #8, #93, #94.

## Trust boundary and transport

There are three zones: loopback, directly attached LAN, and tailnet. The local
HTTP listener binds `127.0.0.1` (normally 4000); its dashboard, root development
pages, desktop integration, and OAuth callbacks are local. The companion API
listener uses HTTPS (normally 4443) on IPv4 network interfaces. Both share the
same authentication and rate-limit middleware. Each port has ten consecutive
fallback choices; `.runtime_api_port` and `.runtime_https_port` record the actual
ports. Implementation: `pond-server/src/main.rs::run_server`, `ports.rs`, and
`pond-api/src/lib.rs::build_transport_router`.

TLS terminates on the Pond with a persisted ECDSA P-256 key. The native companion pins the
public key delivered through the local v2 pairing QR and still checks certificate
validity and hostname SANs. Annual certificate renewal retains the key; replacing
the key requires local re-pairing. WireGuard protects remote transport underneath
HTTPS. Headscale node certificate issuance is not required. See
[remote access](remote-access.md) for deployment and certificate recovery.

### The loopback listener (2026-09-30)

Binding `127.0.0.1` keeps other machines out but not other software on this one.
Three checks close the rest (`pond-api/src/host_guard.rs`, wired outermost on the
dashboard router in `pond-server/src/listeners.rs::compose`):

- **Host.** A request whose `Host` (or HTTP/2 authority) is not `127.0.0.1`,
  `localhost` or `[::1]` gets `421 loopback_host_required`. This defeats DNS
  rebinding, where a page on `attacker.example` re-points its name at `127.0.0.1`
  and then reads and writes the API as a same-origin page. Any port is accepted, so
  `ssh -L 9000:localhost:4000` still works.
- **Origin.** A present `Origin` must be a CORS origin (below) or `http://` on a
  loopback name, else `403 origin_not_allowed`. This defeats cross-site `POST`s
  that need no CORS preflight.
- **Host credential.** Pairing codes and remote-access management
  (`/handshake/pairing-code`, `/remote-access*`) also need
  `X-Pond-Host-Credential`: 32 random bytes the server writes to
  `<data>/.runtime_host_credential` (`0600`, atomically replaced) at every start,
  before `.runtime_api_port`. A pairing code is a full device token, and the
  management routes can re-point the Pond and every phone at another coordinator,
  so reaching loopback is not enough: another OS account on the Pond machine cannot
  read the file. Refusals are `403 host_credential_required`.

The desktop app reads the credential over IPC (`host_credential`). A browser gets it
from the sign-in link `pond-server dashboard` prints,
`http://localhost:<port>/#host=<credential>`: the fragment is never sent over the
network or logged, and the page moves it into that tab's session storage and out of
the address bar. The link stops working at the next restart. A new link opened in a
tab that is already running is taken too (2026-10-05): it differs only after `#`, which
does not reload the page, so the page listens for the change, keeps the new credential
and reloads. A tab refused for want of the current credential says so and stops polling
for recovery requests; before, it asked every five seconds and the Pond logged a
`host_credential_rejected` warning each time, for as long as the tab stayed open.
`pond-server pairing`
reads the file itself. `POND_DEV_ALLOW_LOOPBACK` admits a missing credential for
local development; the Host and Origin checks still apply.

Software running as the Pond's own OS account can read the credential; no
application-level control protects against that, and none is claimed.

Do not publish the listener on the public internet. An untrusted tailnet node
can reach public API routes; encrypting transport does not authenticate its user.
This branch also includes W3 authorization checks for session revocation,
device-scoped notifications and public-route exposure, described below. Those
checks do not prove possession of a device private key. Non-API pages are absent
from the companion router.

## Embedded networking and household authority

Remote access is explicit opt-in. Local-only companions do not contact Headscale.
The Pond's separate persistent Ed25519 authority key signs expiring, single-use
registration approvals. The enrollment service admits a household that registers
its own key, but only with an operator-issued, single-use invite (2026-09-30), and
maps approvals to that household. The invite bounds who can consume the service's
coordinator users and addresses; it does not separate households. The service
supplies default-deny ACLs, which are what separate them, and keeps administrative
credentials private. Approved phones can reach only their own Pond's companion
HTTPS port; phone-to-phone, cross-household, subnet and exit-node access have no
allow rule. Coordinator inventory drift removes permissions rather
than silently accepting a changed device identity.

A Pond the operator imaged is admitted by a device certificate instead of an invite
(2026-10-05). It holds a device key in `embedded-network/device`, beside its
household authority, used only to bind that certificate to its household's first
registration. Keeping the device key out of backups lands with the Pond-side change;
a restored Pond does not need it, because its household is already registered and
registering again needs neither a certificate nor an invite.

The enrollment service holds at most 4096 enrolled devices across every household
(`EnrollmentCapacity`) and refuses further enrollments with `capacity`. It reads the
coordinator inventory node by node and refuses it only past 8192 nodes (`MaxNodes`),
so policy updates, and therefore revocations, keep working at capacity (2026-09-30).
A coordinator it cannot reconcile at startup leaves it serving but degraded, which
`/health` reports as `"degraded":true` until a reconciliation succeeds. Rate limits
key on the client address from the gateway's last `X-Forwarded-For` hop, believed
only when the peer is inside a `--trusted-proxy` range; without that flag every
client behind the gateway shares one budget.

Phones may start from a cached network map; the Pond never does. Its helper sets
`TS_USE_CACHED_NETMAP=false` before starting its node, so access is always
enforced against the coordinator's live map (2026-10-05). A removed phone's cache
is erased when the coordinator refuses it (`Node.ForgetNetworkMapWhenRefused`).

The bundled helper terminates application TLS on the Pond and forwards into a
private Unix socket. Only that private router accepts its peer-identity header;
public listeners ignore caller-supplied forwarding identity. The shared auth and
rate-limit middleware sees the actual embedded peer. LAN-only pairing and
loopback-only management therefore remain enforced. Remote addresses are published
only after the listener and certificate are ready. The helper forwards only
`/api/v1/` requests, and its coordinator client never uses a proxy from the
environment.

GOTG connects remote endpoints through a credentialed, allowlisted loopback CONNECT
proxy; proxy credentials remain native-only. The Go dialer uses the embedded
WireGuard stack directly and cannot fall back to the host's separate VPN route.
Native networking still verifies HTTPS, SPKI, hostname and certificate dates before
sending Pond HTTP. iOS ATS exceptions are limited to the embedded IPv4/IPv6 ranges,
and the native URL protocol intercepts all requests to those ranges, rejecting
unconfigured endpoints and plaintext. Unrelated traffic uses platform trust.

See [pilot deployment](../deploy/remote-access/README.md) for provisioning, durable
revocation, approved identity replacement and consistent backup restoration. Losing
the household authority requires its backup or a new household; there is no cloud
recovery override. This transport does not complete broader authorization work.

## Authentication

- All `/api/v1/*` routes require `Authorization: Bearer <session_token>` and are
  rejected with **401** otherwise, **except** the public allowlist: `/health`,
  `/handshake`, `/handshake/{init,verify,refresh,pairing-code}`,
  onboarding routes, and a few local dev/test pages
  (`crates/pond-api/src/middleware/mod.rs::route_exposure`). The allowlist is
  state-scoped rather than flat: each entry in `PUBLIC_ROUTES` carries an
  `Exposure` of `Always`, `HostOnly`, `Authenticated`, `UntilOnboarded`, or
  `UntilOnboardedThenHostOnly`. `Authenticated` keeps revocation available before
  onboarding finishes without making it public. `HostOnly` requires a token from
  network peers; only the actual loopback connection gets the compatibility
  exemption. Missing connection metadata is treated as remote, and forwarding
  headers do not change this classification.
- Tokens are validated against the DB-backed `SqliteHandshakeAdapter`
  (`validate_token`): only unrevoked, unexpired session tokens pass.
- Session tokens expire after 24h; refresh tokens after 30d. Clients rotate via
  `POST /api/v1/handshake/refresh` (rotation revokes the old session).

### Revocation and device-scoped delivery

`POST /api/v1/handshake/revoke` requires the current session bearer token. It
revokes every session and refresh credential for the authenticated device. Session
issuance and refresh rotation use SQLite transactions so concurrent rotation cannot
escape device revocation. The Pond persists its network-revocation queue before
acknowledging success; coordinator outages do not silently discard that work. The
request body does not choose a token: older GOTG clients may continue sending
`{token: ...}`, but only the bearer is used. Missing, expired and revoked bearer
credentials return 401. The endpoint works before and after onboarding.

Revocation and registry membership move together in both directions (2026-09-21).
`DELETE /api/v1/devices/{id}` revokes every live session for that device through
`Handshake::revoke_device` **before** dropping the row, and refuses the delete if
that fails -- `session_tokens.device_id` carries no foreign key and nothing
cascades onto that table, so the row and the credentials were previously
independent and removing a phone from the list left its token validating.
`POST /handshake/revoke` does the inverse: a device that signs out leaves the
registry as well as losing its credentials, so it stops appearing as a device
that is merely offline. Each orders itself so a failure is recoverable: the
delete refuses rather than completing with access live, and the sign-out removes
credentials first so a failed row removal still leaves the device without access.

Removing a device also removes its remote access (2026-09-30). Before its sessions
are revoked, `DELETE /api/v1/devices/{id}` queues the device's tailnet revocation
through `RemoteRevocation::queue`, the same durable queue sign-out uses, and
refuses with `503 revocation_unavailable` if that cannot be written. Previously
only sign-out did this, so a phone removed from the dashboard kept reaching the
Pond from outside. Matter devices never enroll and are not queued.

The queue (`embedded-network/revocations.json`) backs off each entry on its own,
from 30 seconds doubling to six hours. A coordinator that refuses one device no
longer holds up the devices behind it; an unreachable coordinator still stops the
tick, since it is unreachable for all of them. A read or write fault is logged as
`remote_revocation_store_unavailable` and retried; the loop never returns, so it
can no longer end `serve()`. A revocation for a device the coordinator never saw
would leave a tombstone there, so the Pond keeps `enrolled.json`, the phones it
enrolled: recorded devices are revoked outright, and on a household created after
the record existed an unrecorded device (a desktop session, say) is not sent at
all. A Pond that enrolled phones before the record asks the coordinator first and
drops the entry on `enrollment_missing`. Helper calls lock one device at a time,
so a slow coordinator no longer blocks every enrollment and sign-out, and an
enrollment that a revocation overtook reports a conflict rather than success.
Implementation: `pond-server/src/embedded_network/revocation.rs`.

`revoke_device` carries no default on the port. Every other new method there is
defaulted and each of those defaults narrows -- a forgotten override loses a
capability. A default here would do the opposite: `Ok(0)` reads as "revoked
nothing", the caller deletes the row anyway, and the omission widens access while
looking like success.

### Remote access lapses without household presence (2026-09-21)

Remote access is granted because a device was once on the household LAN, and
nothing re-checked that afterwards: a phone that was lost, stolen, or belonged to
somebody who has left kept a working route in indefinitely. A device now renews
its remote access by authenticating from a directly attached LAN peer, recorded
in the auth middleware -- the one place that knows both which device a token
belongs to and that the peer is not on the tailnet. Thirty days
(`LAN_PRESENCE_WINDOW_DAYS`) without that and its remote access is revoked
through the same durable queue every other revocation uses. Local pairing is
untouched; bringing the device home restores it, and the deadline is reported in
the remote configuration so the app warns from a week out.

This is a second gate beside local approval, not a replacement. Approval decides
**who may change** a household's remote identity, which is what stops somebody
briefly on the wifi installing their own. Presence decides **how long an identity
stays valid unattended**. Neither covers the other's case.

Two deliberate omissions, both narrowing. A device with no sighting at all is
never swept: absence of evidence is not evidence of absence. A household that
never enabled remote access is never swept either, and does not contact
coordination to discover it has nothing to revoke. Sightings live in
`presence.json` beside `revocations.json` rather than a `devices` column.

Where sightings are recorded (2026-09-30). Until this date the presence port was
attached only to the `127.0.0.1` dashboard router, so the only device that ever
renewed was the desktop app, and no phone ever lapsed. It is now attached to the
HTTPS companion listener, which is where phones connect from home, and to nothing
else: the tailnet socket's router is built from the companion before presence is
added, so a request over the tailnet cannot count as being at home even if the
middleware's LAN check were wrong. `listener_wiring.rs` asserts all three listeners.
Overlay VPNs that present as Ethernet (ZeroTier `zt*`/`feth*`, NetBird `wt*`,
Hamachi `ham*`, Nebula `nebula*`) are excluded from the LAN by name, as WireGuard
already was, and so are macOS's `gif*`/`stf*` tunnel interfaces
(`network.rs::is_lan_interface`).

Sightings are keyed by the same network-hashed id as the queue and written at most
once an hour per device. The file is read like the other private files (no
symlinks, `0600`, at most 256 KiB and 1024 devices). An unreadable file used to
stop every sweep, which failed open; it is now moved aside as
`presence.json.corrupt-<time>` and every phone in the enrollment record is treated
as last seen when the file was last written, so each still lapses within the window.

`GET /api/v1/notifications/stream?device_id=...` and
`POST/DELETE /api/v1/devices/{id}/push-token` require the target device to match
`Principal.device_id`, obtained by the middleware from `caller_for_token`.
A mismatch (including absent token attribution) returns 403 `device_mismatch`
before queue access, device lookup or push-token mutation. A caller cannot use
these routes to discover whether someone else's device exists. Claims are not
copied into the principal, and smart-home device targets are not confused with
the identity of a companion phone. No IP address binding is added; an issued
session can roam and refresh remotely.

This is bearer authorization, not device-key proof of possession. A stolen
session can still act as its recorded device until expiry or revocation. Refresh
is possession-based; an already open SSE connection is not reauthenticated per
event. Closing already active streams on credential revocation remains a separate
session-lifecycle change; new requests reject revoked credentials immediately. A client with
an expired session must refresh before server-side logout; local credential
removal alone is not a server revocation guarantee.

### Diagnostics and bootstrap

Transcription (`POST /transcribe`) and agent status (`GET /dev/goose`) require
authentication and completed onboarding. `/tts`, `/test`, `/test/speak`, and
non-API dashboard/development pages allow anonymous **loopback** callers only;
remote callers need a valid token. This preserves local desktop/CLI speech.
Health, onboarding status and `/system/info` remain public for connection and
pairing bootstrap; discovery information must never establish trust in a new TLS
key. Existing onboarding and OAuth-specific guards remain in place.

`/system/info` answers by authentication (2026-09-30). Without a valid bearer it
returns only `https_port`, `tailnet_address` and `protocol: 2`, which is what a
phone needs to connect; with one, the full record (host name, LAN address, public
key pin, version, platform). Before this, every tailnet node could read the
household's host name, addresses and software version before pairing. The pairing
material the desktop and `pond-server pairing` put in the QR now comes back with the
code from the host-only `/handshake/pairing-code` as `pairing`. The mDNS record
still advertises the host name, version and pin to the LAN, by design: that is how
discovery works, and the pin it publishes is confirmed against the Pond's screen.

Denials emit a structured `device_mismatch` warning with the operation name;
accepted device checks emit debug events, and successful revocation emits an
info event. Tokens, notification contents and claimed identifiers are excluded
from those events. The API error code is stable for client localization.

## Pairing (how a client gets a token)

The Android and iOS clients use two-phase HMAC pairing; they do not transmit the six-digit code directly:

1. Operator reads the pairing code printed on server startup, or from
   `pond-server pairing` (`GET /api/v1/handshake/pairing-code`, loopback and the
   host credential).
2. Client `POST /handshake/init {client_id,…}` → `{challenge_id, challenge}`.
3. Client computes `mac = HMAC-SHA256(pairing_code, transcript)` and
   `POST /handshake/verify {challenge_id, mac, channel_binding?}` →
   `{session_token, refresh_token, expires_at, server_proof?}`.

### Channel binding (2026-09-21)

A client that reached the Pond over pinned TLS names the key it pinned to in
`channel_binding` and folds it into the transcript. The Pond recomputes with its
**own** pin, so the two agree only when the client is talking to this Pond
directly:

```text
bound     HMAC(code, "goose-pair-client-v1"\0 ‖ challenge ‖ \0 ‖ client_id ‖ \0 ‖ spki)
unbound   HMAC(code, challenge ‖ client_id)
```

and the Pond answers with `server_proof` over the same transcript under
`goose-pair-server-v1`, which only something holding the pairing code can
produce. A client that sent a binding and got no proof back must treat the pair
as failed.

What this buys: the pin no longer has to reach the phone by a trustworthy route,
which is what lets `_pond._tcp.local.` publish it in a `pin` TXT record for the
app to fill in (`mdns_advertiser.rs`) instead of somebody transcribing 51
characters of base64. Someone who intercepts the connection and serves their own
certificate gets a client that MACs over *their* key: relaying that fails the
recomputation (`channel_binding_mismatch`); stripping the binding and relaying
leaves a MAC over a transcript the Pond no longer computes (`invalid_mac`); and
answering the client directly fails the server proof. A wrong pin therefore ends
pairing in a visible failure rather than a successful pair with the wrong Pond.

The binding is required from every peer except loopback (2026-09-30). The desktop
dashboard pairs over loopback HTTP, where there is no certificate and nothing in
the middle, so it may send the unbound transcript; any other peer that omits
`channel_binding` gets `403 channel_binding_required`, recorded as a failed pairing,
without the challenge being consumed. Before this, a phone on the LAN could still
pair unbound, and an interceptor that stripped the binding from a client willing to
send either shape got a proof it could brute-force offline. Computing *either*
transcript needs the pairing code. A binding is rejected outright, before the
challenge is consumed, when it is not this Pond's pin — including when the Pond
has no TLS identity to compare against, so the field can never be advisory. The
five `sqlite_handshake.rs` tests under `// ---- Channel binding` hold each of
those down.

The legacy single-shot `POST /handshake` carries the code in the request body and
so has no MAC to bind. It is refused from any peer but loopback with
`403 legacy_pairing_host_only`; only local scripts use it.

Legacy handshake, initialization, and verification require loopback or a peer
within an active directly attached LAN interface's netmask. Tunnel interfaces,
point-to-point links, and tailnet allocations are excluded. Missing connection
metadata or failed interface classification is denied. Forwarding headers are
ignored. The rejection is HTTP 403 with `pairing_requires_lan`, including remote
completion of a challenge initialized locally. Refresh is still remotely usable.
See `pond-api/src/network.rs` and the handshake handlers in `routes.rs`.

Codes are single-use and expire in 10 min; a challenge expires in 60 s. The adapter hashes codes and tokens for validation; consult `sqlite_handshake.rs` for the storage contract.

**There is no failed-attempt lockout, and its absence is deliberate.** A bad MAC
burns the *challenge*, not the code: `verify_handshake` consumes the challenge
before it checks the MAC (`sqlite_handshake.rs:355-371`) and returns
`invalid_mac` with the pairing code untouched (`:414-419`), which is consumed
only on success (`:435-439`). The `pairing_codes` table has no attempt counter,
and `bad_mac_attempts_do_not_lock_out_pairing_code` (`:955`) asserts a
legitimate pairing still succeeds after ten bad guesses. Pairing is now limited to direct LAN peers. A lockout would still hand
any guest on the wifi a denial of service against the operator's own pairing.

### Token contract

`HandshakeResponse` uses **`session_token`**, **`refresh_token`**, and
**`expires_at`** (absolute RFC3339). We standardised on absolute expiry rather
than relative `expires_in` to avoid clock-skew / round-trip drift; server,
GOTG, and desktop clients all use this shape.

## Loopback

The blanket loopback auth bypass was **removed** (#94). By default, even
same-host clients (including the desktop app) must present a valid token —
the desktop auto-pairs via the host-only `pairing-code` endpoint, so a browser tab
must first be opened from the `pond-server dashboard` sign-in link.

For local development you can opt back into the bypass with:

```
POND_DEV_ALLOW_LOOPBACK=1
```

This is **off by default** and intended only for dev machines.

## Development switches (2026-10-05)

Every switch below weakens a boundary described above. None is on by default, and
none belongs on a household's Pond. Grep for the variable name rather than
trusting a line number.

| Variable | Effect | Release binary |
|---|---|---|
| `POND_DEV_ALLOW_LOOPBACK=1` | Tokenless requests from loopback peers pass the auth middleware ([Loopback](#loopback)). | Honoured: it is not build-gated. |
| `POND_DEV_INSECURE_LAN=1` | Opens a plaintext HTTP listener for the companion API on every interface (below). | Ignored, with a WARN. |
| `POND_DEV_INSECURE_LAN_PORT` | Port for that listener; default 4080, bound exactly, no fallback. | Ignored with it. |
| `POND_DEV_SAME_MACHINE_MESH=1` | Mesh dials allocate a new port so two Ponds can run on one machine. | Honoured: it is not build-gated. |
| `POND_NETWORK_BINARY` | Path of the embedded-networking helper instead of the bundled `pondnet`. | Ignored in release builds; debug builds honour it. |

### The insecure development listener

Expo Go has no native module to pin the Pond's TLS key, so the companion app
running inside it cannot use the HTTPS listener at all. For that workflow only, a
**debug** build started with `POND_DEV_INSECURE_LAN=1` serves the companion API
over plain HTTP on `0.0.0.0:4080` (or `POND_DEV_INSECURE_LAN_PORT`). The decision
is one function, `pond-api/src/insecure_dev.rs::decide`, and needs both the
variable and `cfg(debug_assertions)`; a release binary with the variable set logs
`kind = "insecure_dev_lan_ignored"` and opens nothing. In a debug build a value
other than `1`, `0` or empty, or a bad port, stops startup rather than quietly
running without it.

What it exposes: everything on the listener is plaintext and unpinned. Anyone on
the network can read every request and response, bearer and refresh tokens
included; can replay those tokens against the HTTPS listener, where they are just
as valid; can impersonate the Pond to the phone and the phone to the Pond; and can
recover the six-digit pairing code offline from a single captured pairing, because
an unbound MAC over plaintext is a dictionary of a million entries. Use it on a
network you control, with a Pond holding no household data you mind losing, and
re-pair over HTTPS before trusting the device again.

What it serves: the companion router, with the same bearer-auth, rate-limit and
LAN-peer pairing guards as the HTTPS listener and the remote-revocation hook, so a
sign-out over it still revokes the phone's tailnet node. It never serves the
dashboard or its management routes, never the remote-access enrollment routes
(a node's enrollment never crosses plaintext), and it is not the tailnet socket.
mDNS does not advertise it; the developer types its address into the app.

Each request on it carries the `InsecureDevTransport` extension, inserted by the
router rather than derived from anything the client sent. Pairing over it is
unbound by construction (there is no TLS key to bind), logs
`kind = "insecure_dev_pairing"`, and the `auth.device_paired` /
`auth.pairing_verify_failed` audit event carries `transport = "insecure_dev"`.
Unbound pairing is otherwise refused off loopback with `channel_binding_required`;
the plaintext listener's marker is the one other exemption. The HTTPS listener's
behaviour is unchanged.

While it runs, `GET /api/v1/system/info` includes `"insecure_dev": true` on every
listener (anonymous or authenticated); the key is absent otherwise. The process
logs `kind = "insecure_dev_lan"` with the port at startup and every ten minutes,
and `serve` prints the address beside the others.

## CORS

Scoped to first-party origins (`app://giap`, `http://localhost:1420`,
`http://127.0.0.1:1420`); **not** `Any`. The same list feeds the loopback
listener's Origin check, and it also covers the remote-access management routes,
which the desktop renderer calls cross-origin. Add extra browser origins (e.g. a LAN
dashboard) with a comma-separated:

```
POND_CORS_ALLOWED_ORIGINS=https://dashboard.lan,https://…
```

`app://giap` is the packaged desktop app. Its renderer is deliberately served
from a privileged custom scheme rather than from `file://`, and the reason is
this list: a `file://` page sends `Origin: null`, which cannot be
allow-listed in any meaningful way and would have forced the allowlist open to
`Any` — undoing the scoping this section exists to describe. The two `:1420`
entries are the Vite dev server, which the desktop shell loads instead of the
packaged bundle during development.

Native mobile clients (GOTG) don't send a browser `Origin` header, so CORS does
not apply to them.

## Rate limiting

Three buckets, not one. The first two are mutually exclusive — handshake traffic
is selected into its own bucket and never touches the general allowance
(`pond-api/src/lib.rs:653-657`).

| Bucket | Limit | Covers | Loopback |
|---|---|---|---|
| General | 600 / 60 s per IP | everything except `/api/v1/handshake*` (`lib.rs:536-539`) | exempt |
| Pairing | 30 / 60 s per IP | every `/api/v1/handshake*` route (`lib.rs:544-547`) | exempt |
| Verify | 10 / 60 s per IP | `/handshake/verify` only (`routes.rs:590-595`) | **not exempt** |

Pairing was split out in `a2c86a93` because a chatty client spending the shared
allowance would lock a device out of `/handshake` — the recovery path. The verify
limiter sits inside the handler rather than the middleware, so it applies to
loopback too: the endpoint is security-sensitive regardless of origin.

The 429 does not have one shape. General and pairing go through
`AuthError::RateLimitExceeded` and set a `Retry-After` **header**
(`middleware/mod.rs:49-53`); verify is built in the handler and puts the same
figure in a `retry_after_secs` **JSON body field** with no header
(`routes.rs:681-688`). A pairing client has to read both, and since verify is
the tighter bucket it is the 429 such a client will actually see. Making the
two uniform is a behaviour change and belongs in its own PR.

Brute force is bounded by that verify limiter plus one-challenge-per-attempt, not
by a lockout. The bound is **per source IP**, like the table above: the limiter
keys on the TCP peer address (`routes.rs:676-678`), so every distinct address
gets its own bucket. At 10 attempts / 60 s against a code that lives 10 minutes,
one address is worth roughly 100 guesses out of 1,000,000 — about 0.01% of the
key space — and each guess burns its own challenge, so attempts cannot be
pipelined. An attacker holding N addresses gets 100N. That matters for the threat
model named above: a wifi guest cannot spoof a source address through a TCP
handshake, but can hold several without effort — a second DHCP lease, a static
address in the subnet, or IPv6 privacy addresses, which rotate on their own. On a
typical /24 the worst case is nearer 2.5% of the key space than 0.01%.

## Residual risks and deferred authorization work

A stolen bearer token is usable until expiry or revocation; session validation
is not proof of possession of a device key. Refresh remains possession-based
with a 30-day window. A compromised tailnet member can reach the API's existing
public allowlist, including some information and development endpoints. Rate
limiting and transport encryption do not remove those authorization risks.
W3 must review them and update allowlist drift tests in the same change.

Native Android and iOS implement public-key pinning in this milestone. Browser
pinning is not implemented. iOS simulator transport verification passes; physical
iPhone roaming remains deferred until a device is available. Source-masking proxies/subnet routers cannot be
used to establish that a pairing peer is local. A compromised Pond OS or phone
can expose private keys or credentials; pinning cannot protect either endpoint
from its own compromise.
