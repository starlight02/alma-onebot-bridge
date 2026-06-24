#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "$ROOT/scripts/version-lib.sh"

TARGET="${TARGET:-x86_64-pc-windows-gnu}"
PROFILE="${PROFILE:-release}"
DIST_DIR="${DIST_DIR:-$ROOT/dist/windows-zip}"
VERSION="$(read_cargo_version "$ROOT")"
PAYLOAD_DIR="$ROOT/dist/windows-$TARGET-$PROFILE"
ZIP_PATH="$DIST_DIR/AlmaOneBotBridge-$VERSION-windows-x86_64.zip"

case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*)
    if command -v pwsh >/dev/null 2>&1; then
      exec pwsh -NoLogo -NoProfile -ExecutionPolicy Bypass -File "$ROOT/scripts/package-windows-zip.ps1"
    fi
    if command -v powershell.exe >/dev/null 2>&1; then
      exec powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -File "$ROOT/scripts/package-windows-zip.ps1"
    fi
    echo "error: PowerShell was not found; run scripts/package-windows-zip.ps1 on Windows" >&2
    exit 1
    ;;
esac

if ! command -v zip >/dev/null 2>&1; then
  echo "error: zip is required to create Windows portable packages" >&2
  exit 1
fi

"$ROOT/scripts/build-windows-cross.sh"

rm -rf "$DIST_DIR"
mkdir -p "$DIST_DIR"

(
  cd "$PAYLOAD_DIR"
  zip -X -r "$ZIP_PATH" .
)

(
  cd "$DIST_DIR"
  shasum -a 256 "$(basename "$ZIP_PATH")" > "$(basename "$ZIP_PATH").sha256"
)

echo "Built Windows ZIP package:"
echo "  $ZIP_PATH"
