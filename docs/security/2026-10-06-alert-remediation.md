# Security alert remediation — 6 October 2026

## Scope and acceptance

This change remediates the three open CodeQL findings and 11 dependency findings.
It tracks all 18 Dependabot findings
reported on `jarida-io/goose-in-a-pond` main. The starting revision is
`b5af5a1c`, which includes the fetched fork main `3dad8d34`.

Acceptance requires regression checks for the reported code paths, patched
resolved dependency versions, frontend and Rust compatibility checks, and an
explicit record of remaining vulnerabilities. Local fixes do not close GitHub
alerts: the changes must land and GitHub must analyze the resulting default branch.
No alerts are dismissed and no new audit suppressions are added.

## Code findings

| Alert | Change | Evidence |
| --- | --- | --- |
| CodeQL #5, weather-card XSS | Escape precipitation before HTML insertion using the existing escaping helper; explicitly stringify values so zero remains visible. | Regression reproduced an injected image before the fix. Tests cover five tool-result fields, decimal values, and zero. A Chromium check exercises the actual tool-result notification. |
| CodeQL #13, dynamic player dispatch | Replace the factory lookup with explicit Apple/Spotify branches. | Existing tests cover supported services, unknown names, and prototype names. The previous Map already rejected prototype lookups; explicit calls remove the flagged dynamic invocation. |
| CodeQL #14, hostname substring check | Match the exact refused destination in the `reason` field, including its colon delimiter. | Regression rejects suffix/prefix decoys, paths, unrelated response fields, and malformed responses. This is a live-test assertion, not a production URL allowlist. |

## Dependency findings

| Dependabot alert | Package | Disposition |
| --- | --- | --- |
| #1 | openssl | Updated from 0.10.79 to 0.10.81. |
| #15 | tar | Updated from 0.4.45 to 0.4.46. |
| #24 | esbuild | Scoped tsup override resolves 0.28.2; the install-script allowlist follows the resolved version. |
| #55 | yamux | libp2p 0.57 replaces both previous Yamux versions with 0.14.1. |
| #56, #57 | hickory-proto | libp2p 0.57 resolves 0.26.3. Removed obsolete RUSTSEC-2026-0118/0119 exceptions. |
| #81, #82 | hickory-resolver | libp2p 0.57 resolves 0.26.3. |
| #62 | serde_with | Updated from 3.20.0 to 3.24.0. |
| #79 | aws-smithy-json | Resolved the AWS family together within Rust 1.91.1 compatibility, replacing both JSON instances with patched 0.62.7. This includes moving pre-existing newer Bedrock/SageMaker/Smithy packages back to the same compatible release family; a mixed family failed compilation. |
| #85 | sprintf-js | Removed through a scoped `@electron/get` override to global-agent 4.1.3, which no longer uses the vulnerable logging dependency. Verified Electron's actual proxy initializer against a local HTTP proxy. |
| #65, #66, #67, #78 | rmcp | Requires coordinated protocol-library and optional code-mode dependency upgrades; see remaining work below. |
| #39 | opentelemetry_sdk | Optional `pctx_config 0.1.5` still requires the vulnerable 0.31 line alongside the already patched 0.32.1 instance. |
| #61 | frost-core | The Lightning SDK pins Lightspark's Git fork at `9aaf1b6b9fa3c2c3c2c7c70da83061deda1a9180`, version 2.1.0. The latest inspected SDK tag, 0.26.1, still uses that revision. |
| #80 | braces | No fixed npm release is published. It remains under Matter's optional Bluetooth native tooling: `@matter/nodejs-ble` → bleno/noble/HCI socket → patch-package → find-yarn-workspace-root → micromatch → braces. Removing this path would remove required Bluetooth functionality. |

## Remaining work and validation boundaries

- Coordinate RMCP >=2.1 with Goose and the pctx packages. A local 2.1 trial
  failed with 14 errors in `pond-mcp-server` and 31 in `goose-provider-types`
  (changed content structures, removed audience methods, and enum matching).
  The trial was reverted; the existing 1.5.0 pin is retained. Updating only the
  workspace pin cannot remove the vulnerable 1.x dependency retained by pctx.
- Update pctx's OpenTelemetry 0.31 dependency to a patched release family.
  The inspected newer `pctx_code_mode 0.6.0` still requires `pctx_config 0.1.5`.
- Obtain a fixed Lightspark FROST revision through the Lightning SDK, preserving
  its fork-specific API and protocol compatibility. A registry override is not
  an equivalent replacement for the SDK's Git fork.
- Follow the braces advisory and Matter/Bluetooth tooling releases. Retain
  Bluetooth support rather than hiding the vulnerable dependency from the lockfile.
- Repeat CodeQL and Dependabot analysis after landing. No local CodeQL result is
  claimed here. Cargo audit uses a different advisory database and existing
  repository exceptions, so a passing result does not supersede the GitHub findings.
- Jetson Orin Nano hardware, mixed-version mesh peers, production AWS credentials,
  and actual Electron artifact downloads through a corporate HTTPS proxy require
  separate verification. The local proxy check covers HTTP routing and bootstrap.
- HawkScan DAST could not run: the required `hawk` CLI was absent and no
  `HAWK_API_KEY` was configured. No DAST-clean claim is made.

## Local verification

All commands run on the development Mac with Rust 1.94.1 and Node 24.14.1.
Rust 1.91.1 compatibility is based on Cargo resolution and published dependency
metadata, not a separate build with that compiler. Build caches were reused; source changes
remain isolated on `fix/security-alert-remediation`.

| Check | Result |
| --- | --- |
| Clean desktop install with npm 10, plus Vite and Electron builds | Passed. |
| Core test suite | Passed, including egress inventory checks and doctests. |
| API library tests | 279 passed. |
| Mesh clippy, all targets | Passed. |
| Desktop Vitest suite after dependency updates | 95 files, 1,444 tests passed. Some existing tests log connection-refused/abort messages; there were no failing tests. |
| Desktop TypeScript checks | Passed for renderer and Electron. |
| Desktop Vite and Electron tsup builds | Passed; Vite reports a large-chunk warning. |
| Weather notification in headless Chromium | Passed: markup displayed as text, no injected image or executed handler. |
| Electron downloader through local HTTP proxy | Passed using `@electron/get.initializeProxy()`. |
| Python live-assertion regressions | 2 tests passed, including malformed and decoy responses. |
| Matter tests and TypeScript | 14 files, 241 tests passed; typecheck passed. |
| Production Rust check (`pond-server`, `pond-adapters-goose`, all targets, locked) | Passed after restoring RMCP 1.5.0, against the final lockfile. |
| Optional AWS provider check (`goose`, no default features, `aws-providers,rustls-tls`, locked) | Passed with the aligned AWS family. |
| Mesh tests on libp2p 0.57 | 8 unit tests and 7 real loopback/relay integration tests passed. |
| npm audit, desktop | Zero vulnerabilities. |
| GitHub advisory-range comparison | 11 dependency alerts outside affected ranges; 7 remain. |
| cargo audit, cached database and no yanked check | Zero unsuppressed RustSec vulnerabilities under the existing policy; two obsolete Hickory exceptions removed. This is not proof that the remaining GitHub alerts are fixed. |

The required core test initially found the existing Uber HTTP sender missing from
the egress inventory. Its request path already checks and records egress; the
inventory now includes it so the guard covers that adapter.

## Changed files

- `crates/pond-mcp-server/apps/weather-card.html`: escape the untrusted field.
- `pond-desktop/src/player/adapters/index.ts`: explicit service dispatch.
- `scripts/live_checks.py`: exact refusal-host assertion.
- `pond-desktop/src/player/weatherCard.test.ts` and `scripts/test_live_checks.py`:
  regressions for the reported injection and misleading-response cases.
- `Cargo.toml`, `Cargo.lock`, `pond-desktop/package.json`, and
  `pond-desktop/package-lock.json`: dependency upgrades and scoped overrides.
- `.cargo/audit.toml`: remove fixed Hickory exceptions.
- `crates/pond-core/tests/egress_guard.rs`: classify the tracked Uber HTTP sender.
- This document: alert dispositions, evidence, and remaining work.

## Advisory references

- [AWS JSON recursion](https://github.com/advisories/GHSA-8ffr-xgwf-xj56)
- [sprintf-js precision exhaustion](https://github.com/advisories/GHSA-hp3w-g68c-fv3c)
- [braces stack exhaustion](https://github.com/advisories/GHSA-vfj7-8cjw-p6xm)
