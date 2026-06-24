#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "$ROOT/scripts/version-lib.sh"

TARGET="${TARGET:-x86_64-pc-windows-gnu}"
PROFILE="${PROFILE:-release}"
VERSION="$(read_cargo_version "$ROOT")"
PAYLOAD_DIR="$ROOT/dist/windows-$TARGET-$PROFILE"

case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*)
    if command -v pwsh >/dev/null 2>&1; then
      exec pwsh -NoLogo -NoProfile -ExecutionPolicy Bypass -File "$ROOT/scripts/package-windows-velopack.ps1"
    fi
    if command -v powershell.exe >/dev/null 2>&1; then
      exec powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -File "$ROOT/scripts/package-windows-velopack.ps1"
    fi
    echo "error: PowerShell was not found; run scripts/package-windows-velopack.ps1 on Windows" >&2
    exit 1
    ;;
esac

"$ROOT/scripts/build-windows-cross.sh"

cat <<EOF
Prepared Windows app payload:
  $PAYLOAD_DIR

Version:
  $VERSION

Velopack per-machine MSI packaging must run on Windows:
  pwsh -File scripts/package-windows-velopack.ps1

Portable ZIP packaging is available from macOS/Linux:
  ./scripts/package-windows-zip.sh

The Windows script uses:
  vpk pack --msi --instLocation PerMachine

Default MSI scope:
  Per-machine under Program Files, using app id AlmaOneBotBridge as the default folder name.
EOF
