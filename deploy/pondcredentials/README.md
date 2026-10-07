# pondcredentials deployment

## Scope and trust boundary

This is one small service that holds **one Apple MusicKit key** and answers **one question**: a
signed developer token, good for 30 days. It exists so a household can play Apple Music without
opening the Apple developer portal.

What it holds: the key, in a file mounted read-only. It is never in an environment variable, an
image, a log line or a response.

What it learns: that some address asked for a token. It does not read a body, set a cookie, or ask
who anyone is. It keeps no address: the rate limit is memory only, and the health counters are two
numbers. Caddy has no access log, on purpose.

What a token is worth: **anyone who can reach the service can get one**, exactly as anyone can read
the token out of any MusicKit web page. A token is not a user's account: playing still needs that
person's own Apple Music subscription and sign-in, which never leaves their pond. So the only abuse
control is the per-address rate limit (20 a minute by default) and, in the end, revoking the key.

The service is not a public offer of Jarida's key to third-party software: it is run so official
ponds need no key. Anyone can still call it; that is the limit above, stated once.

The image is pinned by digest, runs as a non-root user with no shell, on a read-only filesystem
with every capability dropped. The service's own port is on the private Compose network; only
`POST /v1/musickit/developer-token` is reachable from outside.

## What you need

1. **A MusicKit key from Apple** (Certificates, Identifiers & Profiles, Keys, with MusicKit ticked
   and a Media ID chosen). Apple lets a team hold two such keys. **Make a separate key for this
   service** and keep the one you use for testing; see "Rotating" for why the two matter.
2. **A DigitalOcean account and its command line** (`doctl`).
3. **A domain you control**, so the service can have a certificate. A subdomain is fine.

## Create the droplet

Nothing here needs the Apple key yet. **The DigitalOcean API token stays yours**: `doctl auth init`
asks for it, and no assistant should be given it. (An assistant may use a `doctl` you have already
signed in, on your say-so; it should list what it will create and its price first.)

```bash
doctl auth init
doctl compute size list --format Slug,Memory,VCPUs,Disk,PriceMonthly
```

The service is a few MB of memory and one signature a day. `s-1vcpu-512mb-10gb` ($4 a month at the time
of writing) runs the service, Caddy and Docker with room to spare (about 250 of 458 MB in use); the
cloud-init file adds swap for the moments when it does not. **Do not build the image on the droplet**
(compiling Rust in 512 MB fails); build it on your own computer, below.

**Get your key onto the droplet without the emailed password.** A droplet created without a
DigitalOcean-registered SSH key gets a root password emailed to you that must be changed at first login,
and that expiry blocks key logins too (there is no terminal to change it in). `cloud-init.yaml` clears the
expiry and locks the password. Give it your public key in a private copy (a public key is not a secret, but
do not commit yours), or pass a registered key with `--ssh-keys` and skip this:

```bash
{ cat deploy/pondcredentials/cloud-init.yaml; printf 'ssh_authorized_keys:\n  - %s\n' "$(cat ~/.ssh/<YOUR_KEY>.pub)"; } > /tmp/cloud-init.private.yaml
doctl compute droplet create pondcredentials \
  --region fra1 --size s-1vcpu-512mb-10gb --image ubuntu-24-04-x64 \
  --user-data-file /tmp/cloud-init.private.yaml --tag-name pondcredentials --wait
```

`fra1` is an example: pick a region near your households. The first boot takes a minute or two.

Lock the droplet down before anything runs on it: SSH only from your address, HTTP and HTTPS from anywhere
(Caddy needs port 80 to obtain its certificate).

```bash
doctl compute firewall create --name pondcredentials --droplet-ids <DROPLET_ID> \
  --inbound-rules "protocol:tcp,ports:22,address:<YOUR_IP>/32 protocol:tcp,ports:80,address:0.0.0.0/0,address:::/0 protocol:tcp,ports:443,address:0.0.0.0/0,address:::/0" \
  --outbound-rules "protocol:tcp,ports:all,address:0.0.0.0/0,address:::/0 protocol:udp,ports:all,address:0.0.0.0/0,address:::/0"
```

If your address changes you lose SSH until you edit that rule (`doctl compute firewall update`).

Point the name at the droplet. **If your DNS is behind a proxy (Cloudflare's orange cloud), turn the proxy
off for this record**: a proxy sits between every household and the service, so the per-client rate limit
and the "no address is kept" promise would no longer be about the households. Start Caddy only once the
name resolves, or it retries against Let's Encrypt and can hit its failed-validation limit.

```bash
dig +short A <YOUR_DOMAIN> @1.1.1.1        # must print the droplet's address
```

## Ship the service

Build the image for the droplet's architecture on your own computer, copy it over, and copy the two config
files. From the repository root:

```bash
docker build --platform linux/amd64 -f deploy/pondcredentials/Dockerfile -t pondcredentials:local services/pondcredentials
docker save --platform linux/amd64 pondcredentials:local | gzip | ssh root@<DROPLET_IP> 'gunzip | docker load'
scp deploy/pondcredentials/compose.yaml deploy/pondcredentials/Caddyfile root@<DROPLET_IP>:/opt/pondcredentials/
```

The first build takes several minutes on an Apple-silicon Mac (it emulates x86-64) and a few seconds after
that. Then, on the droplet:

```bash
ssh root@<DROPLET_IP>
cd /opt/pondcredentials
mkdir -p runtime/secrets runtime/caddy-data runtime/caddy-config
printf 'CREDENTIALS_HOST=<YOUR_DOMAIN>\nAPPLE_TEAM_ID=<TEAM_ID>\nAPPLE_KEY_ID=<KEY_ID>\nTOKEN_TTL_DAYS=30\nRATE_LIMIT_PER_MINUTE=20\n' > .env
chmod 600 .env
```

The Team ID and Key ID are public configuration, not secrets. The Key ID is the ten characters in the key's
file name (`AuthKey_XXXXXXXXXX.p8`); the Team ID is under Membership details on Apple's developer site.

Then, **from your own computer**, copy the key on. It never goes through this repository or an assistant:

```bash
scp ~/Downloads/AuthKey_XXXXXXXXXX.p8 root@<DROPLET_IP>:/opt/pondcredentials/runtime/secrets/apple.p8
```

Back on the droplet, hand the file to the container's user and start. The image is already loaded, so
`--no-build`:

```bash
chown 65532:65532 runtime/secrets/apple.p8 && chmod 0400 runtime/secrets/apple.p8
docker compose up -d --no-build
```

Settings are read at startup. After editing `.env`, `docker compose restart` is **not** enough:
`docker compose up -d --no-build --force-recreate pondcredentials`.

## Optional: the Uber sign-in relay

Ponds connect a member's Uber account through Jarida's Uber app, whose client secret Uber requires
for every sign-in and token refresh. This service holds it and relays those two requests; it stores
and logs no member token. Turn it on only once Jarida has an Uber developer app.

Add the client ID (public, not a secret) to `.env`, then, **from your own computer**, copy the secret on:

```bash
echo 'UBER_CLIENT_ID=<CLIENT_ID>' >> .env                                  # on the droplet
scp uber_client_secret root@<DROPLET_IP>:/opt/pondcredentials/runtime/secrets/uber_client_secret
```

Register `http://127.0.0.1:<port>/api/v1/oauth/callback` in the Uber app for each port ponds use, or
that pond's sign-ins are refused: 4000 to 4009 for the desktop app and `pond-server serve`, and 8080 to
8089 for the Jetson service, which runs on 8080. A pond takes the next port up when its own is busy,
so a pond started with another `--port` needs that port and the nine after it. Then, on the droplet:

```bash
scp deploy/pondcredentials/compose.uber.yaml root@<DROPLET_IP>:/opt/pondcredentials/   # from your computer
chown 65532:65532 runtime/secrets/uber_client_secret && chmod 0400 runtime/secrets/uber_client_secret
docker compose -f compose.yaml -f compose.uber.yaml up -d --no-build --force-recreate pondcredentials
curl -s https://<YOUR_DOMAIN>/v1/uber/client                             # {"client_id":"..."}
```

Without the overlay the Uber routes answer 503, and nothing else changes.

## Check it

```bash
curl -s -X POST https://<YOUR_DOMAIN>/v1/musickit/developer-token | head -c 160     # a token
curl -s -o /dev/null -w '%{http_code}\n' https://<YOUR_DOMAIN>/healthz                 # 404: not public
docker compose logs pondcredentials      # the key id and lifetimes; never the key, never an address
docker compose exec gateway wget -qO- pondcredentials:8080/healthz                     # the two counters
```

The check that matters is Apple's. Take the token the service returns and ask Apple's catalog for a song
(a garbage token gets a 401, which is your control):

```bash
TOKEN=$(curl -s -X POST https://<YOUR_DOMAIN>/v1/musickit/developer-token | python3 -c 'import json,sys;print(json.load(sys.stdin)["token"])')
curl -s -H "Authorization: Bearer $TOKEN" "https://api.music.apple.com/v1/catalog/us/search?term=coltrane&types=songs&limit=1" -w '\n%{http_code}\n' | tail -c 200
```

The service refuses to start with a missing setting or a key that cannot sign, and says which.

## Pointing ponds at it

Every pond already points at `https://credentials.jarida.io`: that address is `DEFAULT_MANAGED_URL` in
`crates/pond-api/src/musickit.rs`, set on 2026-09-29 and pinned by a test. A pond uses the service only
when the household has stored no key of its own, and every call goes through `network_mode` and shows in
the pond's egress log as `giap-credentials`. A pond asks only after its owner presses Sign in to Apple
Music (or has signed in before), and keeps the token in its secret store, so a restart does not ask again:
about one call every 24 days for a household that uses it, and none for one that does not.

To point a pond at another deployment, set `POND_CREDENTIALS_URL=https://<YOUR_DOMAIN>` in its environment.
To turn it off, set `POND_CREDENTIALS_URL=off`. **A test or a scratch pond must do that**, or it calls this
server: the route tests and `scripts/live-test.sh` set it. If you move the service, change the constant,
and read the privacy notes (`docs/architecture/pondcredentials.md`) first.

## Rotating the key

Apple keeps a token valid until it expires **or its key is revoked**, and a pond only refetches when
its token has a fifth of its life left. So **never revoke first**:

1. Create a new MusicKit key. Put it on the droplet, change `APPLE_KEY_ID` in `.env`, and
   `docker compose up -d`. New tokens are signed by the new key at once.
2. Wait 30 days, the life of the last token signed by the old key.
3. Revoke the old key in Apple's portal.

## If the key leaks

Revoke it in Apple's portal now and deploy a new one as above. Every pond holding a token signed by
the revoked key then fails at Apple until it refetches, which can be up to about three weeks for a
token fetched a week ago. That window is the price of a monthly call, and the reason a fast rotation
is not free; if it proves too long, lower `TOKEN_TTL_DAYS` and accept more calls.

## Not verified

The Docker image has not been built and the compose stack has not been started: the machine this was
written on had no running Docker daemon. What was run: the service's tests and its real binary, the
exact `cargo build --release --locked` the Dockerfile uses, `docker compose config`, a real pond
fetching a real token from the real binary, and the `doctl` flag names against `doctl`'s own help.
The Caddyfile and every `doctl` command are unrun, and no droplet exists.
