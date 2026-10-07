# Private Cargo dependency access in CI

The Rust job fetches the private `jarida-io/llama-cpp-rs-giap` dependency before
building. A repository or organization Actions secret named `CARGO_GITHUB_TOKEN`
must hold a fine-grained token with read-only Contents access to that repository.
The default `GITHUB_TOKEN` cannot read a different private repository.
Both Rust and live-test jobs also accept the existing `GIAP_PRIVATE_FORK_TOKEN`
name as a fallback; new configuration should use `CARGO_GITHUB_TOKEN`.

The credential is available only to the fetch step. `git-askpass.sh` answers
only prompts for that exact HTTPS repository path; it fails closed for other
hosts and paths. Git credential persistence is disabled for that step, and the
token is not embedded in URLs or written into Git configuration. Cargo's CLI
Git fetching is required so the helper is honored.

Fork PR workflows cannot access repository secrets. A maintainer must review
such changes before testing them on a trusted repository branch. This workflow
continues to use `pull_request`, never `pull_request_target`.

Run `python3 scripts/ci/test_git_askpass.py` and
`bash -n scripts/ci/git-askpass.sh` to validate routing with synthetic data.
The tests never consume an existing shell credential. Successful local tests
do not prove that GitHub's secret exists, is authorized by the organization, or
has permission to read the dependency; the hosted fetch step establishes that.

The CI pull-request event covers stacked PRs as well as main-targeted PRs so
security dependency stacks receive the same checks before their bases merge.

Security scans also run for stacked PRs and explicitly cover the Goose
standalone lockfile and Matter lockfile, which previously fell outside the
OSV job. The integration branch includes the separately reviewed Matter fix
so the expanded scanner validates the complete set of remediations.
