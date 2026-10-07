# pondcredentials: managed credentials for Apple Music

A household should not need an Apple developer account to play a song. `pondcredentials` is the
small service that lets that be true: it holds one MusicKit key and hands ponds a signed developer
token. Status, 2026-09-29: **deployed and verified at `credentials.jarida.io`** (one 512 MB droplet in
Frankfurt), and **on by default**: `DEFAULT_MANAGED_URL` is `https://credentials.jarida.io`, which was
Jerry's decision on 2026-09-29 (see "It is on").

## Why a service, and not a token in the release

The pond already ships Spotify's client id in the binary (`bundled_client_id`), and that is fine
because an OAuth PKCE client id is not a secret. Apple's key is a secret, so the equivalent is to
ship a *token*. That is simpler and has no server. It was rejected for two reasons:

- **Revocation is an outage.** If the key is revoked, a shipped token stays dead until every household
  updates. With a service, a new key is deployed once and ponds refetch.
- **It cannot be rate limited.** A token in a release can be copied and used without limit; a service
  can at least slow one address down.

The cost of the service is real: something to run and pay for, a Jarida host every pond can reach,
and a privacy footprint, below.

## The parts

| Part | Where | Job |
|---|---|---|
| Shared signing crate | `services/pondcredentials/token` (`pond-apple-token`) | Normalises a pasted `.p8`, signs the ES256 token, refuses a lifetime past Apple's ceiling. Used by **both** sides, so they cannot drift. |
| The service | `services/pondcredentials/server` | `POST /v1/musickit/developer-token`. Fails to start on a missing setting or a key that cannot sign. |
| The pond's client | `crates/pond-api/src/musickit.rs` | A stored local key wins; else, when a service address is set, fetch, cache, and serve. |
| Uber sign-in relay | `services/pondcredentials/server/src/uber.rs` | Optional. `GET /v1/uber/client`, `POST /v1/uber/token`, `POST /v1/uber/refresh`: adds Jarida's Uber client secret to a pond's code exchange or refresh and returns Uber's tokens. Off (503) unless `UBER_CLIENT_ID` and `UBER_CLIENT_SECRET_FILE` are set. |
| The deploy kit | `deploy/pondcredentials/` | Dockerfile, compose (plus `compose.uber.yaml` for the relay), Caddy, and a runbook with the `doctl` steps. |

The service lives in its own Cargo workspace so a Docker image can copy only that directory, without
the goose submodule and the pond's native engines.

## How a pond uses it

`GET /musickit/developer-token` (the player page's route) signs locally if the household stored a
key. Otherwise, if `POND_CREDENTIALS_URL` names a service, it asks that:

- **Once a month.** The token lives 30 days and is refetched when a fifth of its life is left (or under
  a day), so a household holds weeks of validity at all times.
- **Through the gate.** The fetch is `egress::begin_as(.., "giap-credentials")`: refused under
  `network_mode = offline` before anything is sent, and recorded in the egress log under that name.
- **Kindly to a service that is down.** A failure stands for a minute so the player's ten-second retry
  does not hammer it, and a token still inside its life is served if a refresh fails.
- **Suspiciously.** The reply must be a three-part token with an expiry Apple could have issued
  (not past 200 days), and the address must be `https` (loopback is allowed, for testing).

## What it does to the privacy picture

Every pond that uses it calls a Jarida-run host. What that reveals: **an address asked for a token, at a
time.** The service reads no body, sets no cookie, identifies no one, and keeps no address (the rate
limit is memory only; the counters are two numbers; Caddy has no access log). What it cannot promise is
what the hosting provider's network keeps.

**The Uber relay sees more.** Uber requires Jarida's client secret on every sign-in and token
refresh, and a secret cannot ship in every pond, so those two requests pass through here. That means
a member's Uber authorization code, access token and refresh token **pass through this process**.
It keeps none of them: nothing is written, nothing is logged (a refusal logs Uber's error code and
HTTP status only), replies are `Cache-Control: no-store`, and only the four token fields a pond uses
are passed back. It accepts only a pond's own loopback callback as the return address, so it cannot
be used to exchange codes issued to another site. The tokens are kept on the pond.

It is one more outbound path, documented as such in `02-privacy-and-security-guardrails.md`. It is
**on by default**: a pond with no key of its own asks it. `POND_CREDENTIALS_URL=off` turns it off,
`network_mode = offline` refuses it, and a stored local key means it is never asked. There is no switch
for it in the UI yet (an explicit household toggle was offered and declined for now).

**When a pond calls it, stated plainly.** Not until someone wants Apple Music. The player window keeps
Apple's adapter **asleep** until a person presses **Sign in to Apple Music**, or has signed in in this
window before: until then it fetches no token, loads no script, and nothing leaves the pond. It asks the
pond alone (`GET /musickit/developer-token?probe=true`, which never signs and never reaches the network)
whether a token could be had at all, so "not available on this pond" still shows early. Pressing Sign in
wakes it: the pond fetches a token, the window loads Apple's MusicKit script, and Apple's sign-in opens.
The token is **kept in the pond's secret store** (`APPLE_MUSIC_MANAGED_TOKEN`, under an address check, so
a pond pointed elsewhere never serves it), so a restart uses it with no call, and the pond asks again
only when a fifth of its life is left (about every 24 days) or the token is damaged, lapsed or for another
address. If a renewal fails while the kept token still has days on it, the kept one is served. A pond
whose owner never presses Sign in never calls Jarida, and never loads Apple's script.

Seen in the real app on a scratch pond, with the built-in default and no key: at launch, no calls at all;
one press of Sign in produced one call to the credentials service and then Apple's own hosts, and Apple's
sign-in opened; a restart of both pond and app with no finished sign-in made no calls; a restart with a
sign-in remembered loaded Apple's script and made **no** call to the credentials service.

## Threat model

| Threat | Answer | Left over |
|---|---|---|
| The key leaks from the droplet | It is a file, read-only, never in an image, variable or log; the container is non-root, read-only, no shell, no capabilities | Revoking it breaks ponds holding its tokens until they refetch, up to about three weeks. Rotate with an overlap (runbook). |
| Someone collects tokens | A token is not secret: every MusicKit page ships one | Nothing to add; the rate limit slows it |
| Abuse or a flood | Per-address limit (IPv6 by /64, memory bounded), a 1 KB body cap, one route | A wide botnet is not stopped |
| A network attacker swaps the reply | https only, apart from loopback; the reply is validated | Trusting the certificate authority system |
| Third parties running the open-source pond | Nothing stops them calling it | Rate limits, and revoking the key |
| **Apple's terms** | Read, not settled; **on by default anyway** (Jerry, 2026-09-29) | The Developer Program License Agreement (section 2.8) says not to "share access to mechanisms provided to You by Apple for the use of the Services with any third party", except a Service Provider acting solely on your behalf (2.9), and to use the Services only for "Your Covered Products". An "Application" is software developed by you and distributed under your own brand. Jarida's official builds are defensible; independent forks and self-builds calling the service are not covered by anything read. **Ask Apple, or a lawyer, before this is turned on by default.** |

## It is on

`DEFAULT_MANAGED_URL` in `crates/pond-api/src/musickit.rs` is `https://credentials.jarida.io`, and a test
pins it. To point a pond elsewhere, set `POND_CREDENTIALS_URL` to another https address; to turn it off,
set it to `off`. Tests and scratch ponds must say `off` (the route tests and `scripts/live-test.sh` do),
or they phone home.

## Verification

Service and shared crate: 29 tests (config validation, the token's cryptography, the limiter, and the
HTTP behaviour, including that no response can contain key material). The pond's side: 28 route tests
against a real mock service on loopback (fetch once and cache, a local key wins, failure is remembered,
garbage is refused, offline sends nothing and says which setting, an insecure address is ignored). The
**real service binary** was run with a throwaway key and a **real pond** fetched a token from it: the
signature verified against the service's public key, a second ask did not reach the service, and the
pond logged the call as `giap-credentials`.

Deployed and checked against the live service, 2026-09-29: the Dockerfile builds (11.6 MB, non-root,
reproducible layers) and runs on the droplet; Caddy obtained a Let's Encrypt certificate; over the public
name, with certificate verification on, the token endpoint answers 200 over HTTP/2, every other path and
method 404s, a 4 KB body gets 413, plain HTTP redirects; **Apple's catalog API accepts a token fetched
through it (200, a real song) and rejects a garbage one (401)**; a scratch pond with no key stored fetched
a token from the live service through its own route, the second ask was served from its cache, and the pond
logged `network / egress.http` to that host, tool `giap-credentials`. The service uses about 3 MB, Caddy
37 MB, and the droplet about 250 of 458 MB. No log line carries an address or key text.

Not done: uptime monitoring or alerting (a dead service is noticed by nobody), an unattended-restart test
of the compose stack after a reboot, rotating the key on the live droplet (the procedure is in the
runbook, untried), and Apple's answer on the terms above. The deploy key on the droplet has no passphrase
and should be replaced by its owner's own key.
