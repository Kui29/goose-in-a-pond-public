# Lightning share-refresh threshold validation

Breez SDK 0.19.4 pins Lightspark FROST 2.1, affected by
GHSA-wgq8-vr6r-mqxm. The root workspace patches all three FROST crates to
`Exile10/frost` revision `280c861abcc3f00edfae523b393e5dbb3a1eb249`.
This revision merges the upstream 2.2 security release into the SDK's exact
fork revision, retaining its participant-group/adaptor signing changes.

Dependency review: https://github.com/Exile10/frost/pull/1
Advisory: https://github.com/advisories/GHSA-wgq8-vr6r-mqxm

All resolved FROST packages now declare the genuine upstream 2.2 release.
The version change accompanies the actual dealer and DKG threshold checks;
no advisory exception or suppression is introduced. The SDK version and
wallet storage format remain unchanged.

Validation on macOS: the real Lightning adapter compiled with all targets,
its local unit suite passed, and the complete core test suite passed. The
FROST maintenance branch passed 34 integration tests, 3 BIP340 interoperability
tests, 11 key recreation tests and a taproot tweaking test. Independent
review confirmed the Spark source delta was preserved exactly.

One existing opt-in Lightning test remains ignored. Live wallet operations,
Spark operator interoperability and signing on Jetson hardware remain
unverified. Existing keys previously refreshed with unsafe thresholds are
not repaired by this dependency update; consult the upstream advisory.
HawkScan could not run because its CLI and API key are unavailable.
