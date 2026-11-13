# Private Cargo dependency access in CI

The Rust job fetches the `jarida-io/llama-cpp-rs-giap` dependency before building.
That repository is public, so the fetch needs no credential. If it is made private,
a repository or organization Actions secret named `CARGO_GITHUB_TOKEN` must hold a
fine-grained token with read-only Contents access to it; the default `GITHUB_TOKEN`
cannot read a different private repository, and the fetch step names the missing
secret when it fails without one. Both Rust and live-test jobs also accept the
existing `GIAP_PRIVATE_FORK_TOKEN` name as a fallback; new configuration should use
`CARGO_GITHUB_TOKEN`.

The credential is available only to the fetch step. `git-askpass.sh` answers
only prompts for that exact HTTPS repository path; it fails closed for other
hosts and paths. Git credential persistence is disabled for that step, and the
token is not embedded in URLs or written into Git configuration. Cargo's CLI
Git fetching is required so the helper is honored.

Fork PR workflows cannot access repository secrets. A maintainer must review
such changes before testing them on a trusted repository branch. This workflow
continues to use `pull_request`, never `pull_request_target`.

On a fork PR the Rust job is therefore skipped rather than run to a certain
failure. GitHub reports a skipped job as a success, so `Rust — skipped on a fork
PR` fails in its place whenever the PR changes Rust (a `.rs` file, a Cargo
manifest or lock, `crates/`, `vendor/`, `.cargo/`, `Cross.toml` or the goose
submodule), and passes otherwise. Make both checks required together. To merge a
fork PR that touches Rust, push its branch to `jarida-io/goose-in-a-pond` so the
job runs with the secret. Pushes and same-repository PRs always run the job, and it fails
loudly there when the secret is missing. Claude Code Review skips fork PRs for
the same reason, since they get no OIDC token either. Comment `@claude review`
to request a review instead.

Run `python3 scripts/ci/test_git_askpass.py` and
`bash -n scripts/ci/git-askpass.sh` to validate routing with synthetic data.
The tests never consume an existing shell credential. Successful local tests
do not prove that GitHub's secret exists, is authorized by the organization, or
has permission to read the dependency; the hosted fetch step establishes that.

The CI `pull_request` event has no branch filter, so stacked PRs get the same checks as
PRs against `main`. `live-test-gate` skips the live test, with a notice, when neither secret is
set, since nothing in the workspace compiles without the private dependency.

`.github/workflows/security.yml` runs on pull requests, pushes to `main` and weekly: `cargo audit`,
`npm audit`, govulncheck and OSV for `native/pondnet`, gitleaks, and OSV over every lockfile
(`Cargo.lock`, `goose/Cargo.lock`, `pond-desktop`, `extensions/music`, `matter-server` and
`services/pondcredentials`).
