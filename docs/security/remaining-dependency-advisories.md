# Additional dependency advisories found by CI

The October 6 security run found fixed-version advisories beyond the original
Dependabot screenshots. This update removes the remaining affected instances:

| Dependency | Patched resolution or replacement |
| --- | --- |
| anyhow | 1.0.103 |
| event-listener | 5.4.2 |
| memmap2 | 0.9.11 |
| lru 0.16 | Nostr SDK 0.45 removes the old instance; remaining lru is 0.18.2 |
| scc 2.4 | serial_test 3.5 removes SCC without raising the workspace Rust requirement |
| quick-xml 0.36/0.37 | CalDAV uses 0.41; docx-rs 0.4.22 and notify-rust 4.18.1 remove old transitive instances |

The Nostr SDK API adaptation preserves explicit relay selection, the 8-second
connection and 10-second fetch timeouts, NIP-44 encryption, and signed session
events. Its optional TLS features follow the selected Goose backend. Live
public relay publication is not part of local validation.

quick-xml 0.41 emits entity references separately. CalDAV therefore assembles
complete property text before trimming it, preserving href query strings,
spaces, numeric references and CDATA. Invalid entities fail instead of silently
losing data; the existing connector error path reports the failure.

Both GIAP and Goose lockfiles are updated. The standalone Goose scan also
identified older h2, rustls, webbrowser and LRU patch releases, now aligned
with GIAP, plus umya-spreadsheet 3.0.0, now upgraded to 3.0.1 to remove its
old XML parser. Both graphs are scanned separately. The obsolete quick-xml audit ignores
and retired instant/nostr-relay-pool maintenance exceptions are removed. The
OSV job now fails on findings; the existing reviewed, expiring exceptions for
unfixable upstream advisories remain visible in osv-scanner.toml. No new
exception is added. A zero exit under that policy is not an assertion that all
transitive upstream projects are maintained or all advisories have patches.

Local test results and runtime boundaries are recorded in the PRs. Windows
native notifications, live Nostr relays and full Jetson runtime integration
require separate verification. HawkScan is unavailable without its CLI and
API key; hosted Rust CI needs the private Cargo dependency credential.
