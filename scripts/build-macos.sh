#!/bin/bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
MACOS_DIR="$ROOT/platforms/macos"
TARGET="${TARGET:-$(rustc -vV | awk '/host:/ {print $2}')}"
CONFIGURATION="${CONFIGURATION:-Release}"
HOST_ARCH="$(uname -m)"
DESTINATION="${DESTINATION:-platform=macOS,arch=$HOST_ARCH}"

echo "==> Building Xcode project..."
cd "$MACOS_DIR"
xcodebuild \
    -project AlmaOneBotBridge.xcodeproj \
    -scheme AlmaOneBotBridge \
    -configuration "$CONFIGURATION" \
    -destination "$DESTINATION" \
    -derivedDataPath build/ \
    RUST_BRIDGE_TARGET="$TARGET" \
    CODE_SIGNING_ALLOWED="${CODE_SIGNING_ALLOWED:-NO}" \
    | tail -5

APP="build/Build/Products/$CONFIGURATION/AlmaOneBotBridge.app"
APP_RESOURCE="$APP/Contents/Resources/alma-onebot-bridge"

if [[ ! -x "$APP_RESOURCE" ]]; then
    echo "==> Xcode did not copy the bridge binary; installing it into app resources..."
    mkdir -p "$APP/Contents/Resources"
    cp "$ROOT/target/$TARGET/release/alma-onebot-bridge" "$APP_RESOURCE"
    chmod +x "$APP_RESOURCE"
fi

if command -v codesign >/dev/null 2>&1; then
    echo "==> Applying ad-hoc code signature..."
    codesign --force --deep --sign - "$APP" >/dev/null
fi

if [[ "${INSTALL_TO_APPLICATIONS:-0}" == "1" ]]; then
    echo "==> Installing to /Applications..."
    rm -rf "/Applications/AlmaOneBotBridge.app"
    ditto "$APP" "/Applications/AlmaOneBotBridge.app"
fi

echo "==> Done: $MACOS_DIR/$APP"
echo "==> Launch with: open \"$MACOS_DIR/$APP\""
