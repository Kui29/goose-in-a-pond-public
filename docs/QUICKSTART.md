# Quick start for contributors

You can make your first change and check it the way CI will in about ten minutes. Start with the
dashboard (`pond-desktop/`): it needs no special access, no Pond and no Rust build. The full rules
are in [CONTRIBUTING.md](CONTRIBUTING.md).

## 1. Get the code

Fork [jarida-io/goose-in-a-pond](https://github.com/jarida-io/goose-in-a-pond) on GitHub, then:

```bash
git clone https://github.com/<you>/goose-in-a-pond.git
cd goose-in-a-pond
git remote add upstream https://github.com/jarida-io/goose-in-a-pond.git
git fetch upstream
git checkout -b fix/<short-name> upstream/main
```

The dashboard does not need the `goose/` submodule, so skip `--recursive` for now.

## 2. Get Node

You need Node 22 (22.13 or newer), 24, or 26 and newer, as `pond-desktop/package.json` requires.
If `node --version` shows something else:

```bash
bash scripts/giap.sh node
```

It finds a Node in range, or downloads and verifies one.

## 3. Check that everything passes before you change anything

```bash
bash scripts/giap.sh check desktop
```

This installs the dashboard's packages, then runs the type check, the linter and about 1,800 unit
tests. The first run takes a few minutes; later runs take under a minute. It ends with `Passed:` or
`Failed:` and the areas that did.

## 4. Make your change and check it again

The dashboard's code is in `pond-desktop/src/`. To see your change in a browser:

```bash
cd pond-desktop && npm run dev:vite
```

Without a running Pond the dashboard has no data behind it. The browser tests in
`pond-desktop/tests/e2e/` answer its requests with mocks instead, so they need no Pond.

Then run the checks for everything your branch touches:

```bash
bash scripts/giap.sh check          # or: make check
bash scripts/giap.sh check --e2e    # also the browser tests (about 12 minutes)
```

With no area named, `check` compares your branch with `main` and runs what CI would. A
docs-only change has nothing to check.

## 5. Open a pull request

Commit in the [format CONTRIBUTING.md describes](CONTRIBUTING.md#commit-messages), push to your
fork and open a pull request against `jarida-io/goose-in-a-pond` `main`.

## Working on Rust

Every Rust build fetches a private repository, `jarida-io/llama-cpp-rs-giap`, so it needs read
access to it; ask a maintainer. Without access, `giap.sh check rust` stops and says so. With
access:

```bash
git submodule update --init --recursive
bash scripts/giap.sh doctor        # what this machine has and lacks
bash scripts/giap.sh check rust    # fmt, clippy and the fast-crate tests, as CI runs them
```

The first Rust build compiles Goose and llama.cpp and takes a while; later ones are quick.
`--full` also checks the production binary.

CI does not run the Rust job on pull requests from forks, because they get no access to the
private repository. A maintainer runs it by pushing your branch to `jarida-io/goose-in-a-pond`.

## Finding something to work on

Issues labelled
[good first issue](https://github.com/jarida-io/goose-in-a-pond/labels/good%20first%20issue)
say how long they should take and whether they need the private repository.
