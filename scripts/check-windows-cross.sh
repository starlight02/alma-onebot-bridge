#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET="${TARGET:-x86_64-pc-windows-gnu}"

if ! command -v docker >/dev/null 2>&1; then
  echo "docker is required for cross checks" >&2
  exit 1
fi

if ! docker info >/dev/null 2>&1; then
  echo "docker is installed but not running" >&2
  exit 1
fi

if ! command -v cross >/dev/null 2>&1; then
  echo "cross is not installed; run: cargo install cross" >&2
  exit 1
fi

if [[ "$(uname -m)" == "arm64" ]]; then
  # cross-rs images are amd64-only; custom pre-build images need the same
  # platform override as runtime containers on Apple Silicon.
  export DOCKER_DEFAULT_PLATFORM="${DOCKER_DEFAULT_PLATFORM:-linux/amd64}"
  export CROSS_CONTAINER_OPTS="${CROSS_CONTAINER_OPTS:---platform linux/amd64}"
  export CROSS_BUILD_OPTS="${CROSS_BUILD_OPTS:---platform linux/amd64}"
fi

cd "$ROOT"
cross check --target "$TARGET"
