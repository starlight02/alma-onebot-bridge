#!/bin/bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
MACOS_DIR="$ROOT/platforms/macos"
CONFIGURATION="${CONFIGURATION:-Release}"
VERSION="${VERSION:-$(awk -F\" '/^version =/ {print $2; exit}' "$ROOT/Cargo.toml")}"
HOST_ARCH="$(uname -m)"
BUILD_UNIVERSAL="${BUILD_UNIVERSAL:-0}"

if [[ "$BUILD_UNIVERSAL" == "1" ]]; then
    RUST_BRIDGE_TARGETS="${RUST_BRIDGE_TARGETS:-aarch64-apple-darwin x86_64-apple-darwin}"
    ARCHS="${ARCHS:-arm64 x86_64}"
    DESTINATION="${DESTINATION:-generic/platform=macOS}"
    ONLY_ACTIVE_ARCH="${ONLY_ACTIVE_ARCH:-NO}"
else
    TARGET="${TARGET:-$(rustc -vV | awk '/host:/ {print $2}')}"
    RUST_BRIDGE_TARGETS="${RUST_BRIDGE_TARGETS:-$TARGET}"
    if [[ -z "${ARCHS:-}" ]]; then
        case "$TARGET" in
            aarch64-apple-darwin) ARCHS="arm64" ;;
            x86_64-apple-darwin) ARCHS="x86_64" ;;
            *) ARCHS="$HOST_ARCH" ;;
        esac
    fi
    DESTINATION="${DESTINATION:-generic/platform=macOS}"
    ONLY_ACTIVE_ARCH="${ONLY_ACTIVE_ARCH:-NO}"
fi

echo "==> Building Xcode project..."
cd "$MACOS_DIR"
xcodebuild_args=(
    -project AlmaOneBotBridge.xcodeproj \
    -scheme AlmaOneBotBridge \
    -configuration "$CONFIGURATION" \
    -destination "$DESTINATION" \
    -derivedDataPath build/ \
    RUST_BRIDGE_TARGETS="$RUST_BRIDGE_TARGETS" \
    ARCHS="$ARCHS" \
    ONLY_ACTIVE_ARCH="$ONLY_ACTIVE_ARCH" \
    MARKETING_VERSION="$VERSION" \
    CODE_SIGNING_ALLOWED="${CODE_SIGNING_ALLOWED:-NO}"
)

if [[ "${CI:-}" == "true" ]]; then
    xcodebuild "${xcodebuild_args[@]}"
else
    xcodebuild "${xcodebuild_args[@]}" | tail -5
fi

APP="build/Build/Products/$CONFIGURATION/AlmaOneBotBridge.app"
APP_RESOURCE="$APP/Contents/Resources/alma-onebot-bridge"

if [[ ! -x "$APP_RESOURCE" ]]; then
    echo "==> Xcode did not copy the bridge binary; installing it into app resources..."
    mkdir -p "$APP/Contents/Resources"
    cp "$ROOT/target/$TARGET/release/alma-onebot-bridge" "$APP_RESOURCE"
    chmod +x "$APP_RESOURCE"
fi

if [[ -n "${APP_SIGN_IDENTITY:-}" ]]; then
    echo "==> Signing app with Developer ID Application identity..."
    codesign --force --deep --options runtime --timestamp --sign "$APP_SIGN_IDENTITY" "$APP"
elif [[ "${AD_HOC_SIGN:-1}" == "1" ]] && command -v codesign >/dev/null 2>&1; then
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
