# ─────────────────────────────────────────────────────────────────────────────
# GIAP — convenience targets.
#
# The primary interface is the menu-driven control script:
#
#     bash scripts/giap.sh            # install, build, service, logs, doctor
#     bash scripts/giap.sh doctor     # non-interactive health check, exit 1 on FAIL
#
# It detects the host (Jetson / Linux / macOS) and whether CUDA is usable, and
# picks the right feature flags. This Makefile only forwards to it.
#
# Jetson-specific verbs live in scripts/jetson.sh (deploy | build | docker-build
# | optimize | probe-mlc).
#
# The old cross-compilation targets were removed. They did not work for the path
# that matters: nvcc has to target the device architecture, so a CUDA build must
# happen ON the Jetson. They also referenced scripts/build-jetson-native.sh,
# which does not exist, and deployed to a host and directory that no longer do
# either. Use `make deploy` below, which routes through the real deploy script.
# ─────────────────────────────────────────────────────────────────────────────

.PHONY: install build ui server desktop doctor status deploy docker-build test check fmt help

## install       Install GIAP on this host (guardrailed; delegates to install.sh)
install:
	bash scripts/giap.sh install

## build         Build the web UI + pond-server with this host's correct features
build:
	bash scripts/giap.sh build

## ui            Build only the web UI (embedded into the server at compile time)
ui:
	bash scripts/giap.sh build-ui

## server        Build only pond-server (release, host-appropriate features)
server:
	bash scripts/giap.sh build

## desktop       Build the Electron desktop app (macOS only)
desktop:
	cd pond-desktop && npm ci && npm run bundle:app

## doctor        Health check — submodule drift, CUDA build, service scope, disk
doctor:
	bash scripts/giap.sh doctor

## status        Print the detection banner for this host
status:
	bash scripts/giap.sh status

## deploy        Build the UI here, sync it, and build on the Jetson over ssh
deploy:
	bash scripts/jetson.sh deploy

## docker-build  aarch64 CPU-only binary via a linux/arm64 container (no CUDA)
docker-build:
	bash scripts/jetson.sh docker-build

## test          Run the fast-crate test suite (mirrors ci.yml)
test:
	SQLX_OFFLINE=true cargo test -p pond-core -p pond-api -p pond-infra

## check         Run the checks CI runs, for what this branch changes
check:
	bash scripts/giap.sh check

## fmt           Format the workspace
fmt:
	cargo fmt

## help          Show this help
help:
	@grep -E '^## ' Makefile | sed 's/## /  /'
