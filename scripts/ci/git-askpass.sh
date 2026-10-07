#!/usr/bin/env bash
set -eu

# Limit this credential to the private dependency; never answer for another host or repository.
case "${1:-}" in
  "Username for 'https://github.com/jarida-io/llama-cpp-rs-giap':"* | \
  "Username for 'https://github.com/jarida-io/llama-cpp-rs-giap.git':"*)
    printf '%s\n' 'x-access-token'
    ;;
  "Password for 'https://x-access-token@github.com/jarida-io/llama-cpp-rs-giap':"* | \
  "Password for 'https://x-access-token@github.com/jarida-io/llama-cpp-rs-giap.git':"*)
    test -n "${CARGO_GITHUB_TOKEN:-}" || exit 1
    printf '%s\n' "$CARGO_GITHUB_TOKEN"
    ;;
  *) exit 1 ;;
esac
