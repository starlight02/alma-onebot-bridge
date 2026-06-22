#!/bin/bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
MACOS_DIR="$ROOT/platforms/macos"
CONFIGURATION="${CONFIGURATION:-Release}"
VERSION="${VERSION:-$(awk -F\" '/^version =/ {print $2; exit}' "$ROOT/Cargo.toml")}"
IFS=. read -r VERSION_MAJOR VERSION_MINOR VERSION_PATCH _ <<< "$VERSION"
VERSION_MAJOR="${VERSION_MAJOR:-0}"
VERSION_MINOR="${VERSION_MINOR:-0}"
VERSION_PATCH="${VERSION_PATCH:-0}"
BUILD_NUMBER="${BUILD_NUMBER:-$((10#$VERSION_MAJOR * 10000 + 10#$VERSION_MINOR * 100 + 10#$VERSION_PATCH))}"
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
    CURRENT_PROJECT_VERSION="$BUILD_NUMBER" \
    CODE_SIGNING_ALLOWED="${CODE_SIGNING_ALLOWED:-NO}"
)

if [[ "${CI:-}" == "true" ]]; then
    xcodebuild "${xcodebuild_args[@]}"
else
    xcodebuild "${xcodebuild_args[@]}" | tail -5
fi

APP="build/Build/Products/$CONFIGURATION/AlmaOneBotBridge.app"
APP_RESOURCE="$APP/Contents/Resources/alma-onebot-bridge"
APP_ICONSET="$MACOS_DIR/AppIcon.iconset"
APP_ICNS="$APP/Contents/Resources/AppIcon.icns"
APP_INFO_PLIST="$APP/Contents/Info.plist"

echo "==> Installing complete macOS app icon..."
ICON_WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/alma-onebot-icon.XXXXXX")"
trap 'rm -rf "$ICON_WORK_DIR"' EXIT
ICONSET_WORK="$ICON_WORK_DIR/AppIcon.iconset"
ICONSET_CHECK="$ICON_WORK_DIR/AppIcon-check.iconset"
mkdir -p "$ICONSET_WORK"
cp "$APP_ICONSET"/icon_*.png "$ICONSET_WORK"/
iconutil -c icns "$ICONSET_WORK" -o "$APP_ICNS"
iconutil -c iconset "$APP_ICNS" -o "$ICONSET_CHECK"
if [[ ! -f "$ICONSET_CHECK/icon_512x512@2x.png" ]]; then
    echo "error: generated AppIcon.icns is missing the 1024px rendition" >&2
    exit 1
fi
/usr/libexec/PlistBuddy -c 'Delete :CFBundleIconName' "$APP_INFO_PLIST" >/dev/null 2>&1 || true
if /usr/libexec/PlistBuddy -c 'Print :CFBundleIconFile' "$APP_INFO_PLIST" >/dev/null 2>&1; then
    /usr/libexec/PlistBuddy -c 'Set :CFBundleIconFile AppIcon.icns' "$APP_INFO_PLIST"
else
    /usr/libexec/PlistBuddy -c 'Add :CFBundleIconFile string AppIcon.icns' "$APP_INFO_PLIST"
fi

if [[ ! -x "$APP_RESOURCE" ]]; then
    echo "==> Xcode did not copy the bridge binary; installing it into app resources..."
    mkdir -p "$APP/Contents/Resources"
    read -r -a bridge_targets <<< "$RUST_BRIDGE_TARGETS"
    bridge_binaries=()
    for bridge_target in "${bridge_targets[@]}"; do
        bridge_binary="$ROOT/target/$bridge_target/release/alma-onebot-bridge"
        if [[ ! -x "$bridge_binary" ]]; then
            cargo build --release --target "$bridge_target"
        fi
        bridge_binaries+=("$bridge_binary")
    done
    if (( ${#bridge_binaries[@]} == 1 )); then
        cp "${bridge_binaries[0]}" "$APP_RESOURCE"
    else
        lipo -create "${bridge_binaries[@]}" -output "$APP_RESOURCE"
    fi
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
