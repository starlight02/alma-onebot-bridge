#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
target="${TARGET:-x86_64-pc-windows-gnu}"
profile="${PROFILE:-release}"
windows_root="$repo_root/platforms/windows"

if ! command -v docker >/dev/null 2>&1; then
  echo "docker is required for cross builds" >&2
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

if [[ -z "${CROSS_CONTAINER_OPTS:-}" && "$(uname -m)" == "arm64" ]]; then
  export CROSS_CONTAINER_OPTS="--platform linux/amd64"
fi

if [[ -z "${CROSS_BUILD_OPTS:-}" && "$(uname -m)" == "arm64" ]]; then
  export CROSS_BUILD_OPTS="--platform linux/amd64"
fi

shim_link_args=(
  "-C link-arg=/usr/local/lib/gethostnamew_shim.o"
  "-C link-arg=-lws2_32"
  "-C link-arg=-lkernel32"
)
for link_arg in "${shim_link_args[@]}"; do
  if [[ -n "${RUSTFLAGS:-}" ]]; then
    if [[ " $RUSTFLAGS " != *" $link_arg "* ]]; then
      export RUSTFLAGS="$RUSTFLAGS $link_arg"
    fi
  else
    export RUSTFLAGS="$link_arg"
  fi
done

args=(build --target "$target")
if [[ "$profile" == "release" ]]; then
  args+=(--release)
fi

cd "$windows_root"
cross "${args[@]}"

exe="$repo_root/platforms/windows/target/$target/$profile/AlmaOneBotBridge.exe"
if [[ ! -f "$exe" ]]; then
  echo "expected Windows app executable not found: $exe" >&2
  exit 1
fi

payload="$repo_root/dist/windows-$target-$profile"
rm -rf "$payload"
mkdir -p "$payload"

for file in AlmaOneBotBridge.exe Microsoft.WindowsAppRuntime.Bootstrap.dll resources.pri; do
  source="$repo_root/platforms/windows/target/$target/$profile/$file"
  if [[ ! -f "$source" ]]; then
    echo "expected Windows payload file not found: $source" >&2
    exit 1
  fi
  cp "$source" "$payload/"
done

echo "Built Windows app:"
echo "  $exe"
echo "Prepared Windows payload:"
echo "  $payload"
