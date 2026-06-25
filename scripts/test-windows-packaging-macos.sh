#!/usr/bin/env bash
# macOS/Linux local gate for Windows release packaging (everything except vpk MSI).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "$ROOT/scripts/version-lib.sh"

echo "== verify version =="
"$ROOT/scripts/verify-version.sh"

echo "== cargo test (host) =="
cargo test --manifest-path "$ROOT/Cargo.toml"

echo "== cross build Windows tray app =="
"$ROOT/scripts/build-windows-cross.sh"

PAYLOAD="$ROOT/dist/windows-x86_64-pc-windows-gnu-release"
for f in AlmaOneBotBridge.exe Microsoft.WindowsAppRuntime.Bootstrap.dll resources.pri; do
  if [[ ! -f "$PAYLOAD/$f" ]]; then
    echo "error: missing payload file: $PAYLOAD/$f" >&2
    exit 1
  fi
done

VERSION="$(read_cargo_version "$ROOT")"
ZIP="$ROOT/dist/windows-zip/AlmaOneBotBridge-${VERSION}-windows-x86_64.zip"
rm -rf "$ROOT/dist/windows-zip"
mkdir -p "$ROOT/dist/windows-zip"
(
  cd "$PAYLOAD"
  zip -X -r "$ZIP" .
)
(
  cd "$ROOT/dist/windows-zip"
  shasum -a 256 "$(basename "$ZIP")" > "$(basename "$ZIP").sha256"
)

echo "== ok =="
echo "  payload: $PAYLOAD"
echo "  zip:     $ZIP"
echo "MSI (vpk --noPortable --skipUpdates --noInst) must be verified on Windows or CI."
