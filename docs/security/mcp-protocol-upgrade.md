# MCP protocol dependency security update

RMCP 2.1.0 fixes the resource-metadata spoofing/SSRF, unauthenticated HTTP
session leak and cross-origin redirect-header disclosures reported by
Dependabot alerts #65, #66, #67 and #78. The parent workspace and Goose use a
single pinned version. OpenTelemetry SDK 0.32.1 replaces the optional PCTX
0.31 dependency behind alert #39.

The Goose fork ports the upstream RMCP 2 migration from
`7b7b8aa58f54a7e189f681883de55bd42b633634`, keeps the GIAP patches, and adapts
its additional LiteRT conversion path to the flattened content model.
The parent MCP servers and Goose adapter use the same ContentBlock API.
Custom ordinary, OAuth and PCTX HTTP clients also disable redirects explicitly;
supplying a custom client bypasses RMCP's default redirect policy. Local HTTP
regressions prove headers reach the configured endpoint but no request reaches
the destination of 307/308 redirects.

Four vendored PCTX compatibility patches retain code-mode instead of removing
it. The optional feature is compiled explicitly. Provenance, retained licenses
and the small changes relative to published packages are recorded in
`goose/vendor/SECURITY-PATCHES.md`. Both root lockfiles resolve only RMCP 2.1
and OpenTelemetry SDK 0.32.1. No audit exceptions are added.

RMCP now strips top-level input-schema title/description. The 27-tool raw
prefix shrinks from 14,478 to 13,548 bytes; names and ordering remain unchanged.
The prefix fixture records this intentional protocol-library change. Raw byte
counts are not a measurement of the provider's minified prompt token cost.

CI now checks out the committed Goose submodule revision. Following the fork's
main branch would test different code while this coordinated update is in
review. Merge the Goose dependency PR before merging its parent integration.

Validation and boundaries are recorded in the pull requests. Live model turns,
Jetson GPU inference, external OAuth providers and remote MCP services require
separate runtime verification. HawkScan is blocked by its unavailable CLI and
API key. Hosted Rust CI also needs a read-only credential for the existing
private llama.cpp fork; local authenticated builds do not establish that CI
credential is configured. Additional OSV findings outside the original alerts
are not dismissed or hidden by this change.
