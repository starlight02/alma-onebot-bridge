#!/bin/bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP_NAME="AlmaOneBotBridge"
BUNDLE_ID="moe.aili.alma-onebot-bridge"
VERSION="${VERSION:-$(awk -F\" '/^version =/ {print $2; exit}' "$ROOT/Cargo.toml")}"
CONFIGURATION="${CONFIGURATION:-Release}"
DIST_DIR="${DIST_DIR:-$ROOT/dist/macos}"
BUILD_UNIVERSAL="${BUILD_UNIVERSAL:-1}"
PACKAGE_ARCH="${PACKAGE_ARCH:-}"
WORK_DIR="$DIST_DIR/work"
RESOURCES_DIR="$WORK_DIR/resources"
DISTRIBUTION_XML="$WORK_DIR/Distribution.xml"
APP_PATH="$ROOT/platforms/macos/build/Build/Products/$CONFIGURATION/$APP_NAME.app"
STAGING_ROOT="$WORK_DIR/root"
STAGED_APP="$STAGING_ROOT/Applications/$APP_NAME.app"
COMPONENT_PKG="$WORK_DIR/$APP_NAME-component.pkg"

export COPYFILE_DISABLE=1

if [[ -z "$PACKAGE_ARCH" ]]; then
    if [[ "$BUILD_UNIVERSAL" == "1" ]]; then
        PACKAGE_ARCH="universal"
    else
        case "${TARGET:-$(rustc -vV | awk '/host:/ {print $2}')}" in
            aarch64-apple-darwin) PACKAGE_ARCH="arm64" ;;
            x86_64-apple-darwin) PACKAGE_ARCH="amd64" ;;
            *) PACKAGE_ARCH="native" ;;
        esac
    fi
fi

if [[ -n "${INSTALLER_SIGN_IDENTITY:-}" ]]; then
    FINAL_PKG="$DIST_DIR/$APP_NAME-$VERSION-$PACKAGE_ARCH.pkg"
else
    FINAL_PKG="$DIST_DIR/$APP_NAME-$VERSION-$PACKAGE_ARCH-unsigned.pkg"
fi

rm -rf "$WORK_DIR"
mkdir -p "$RESOURCES_DIR" "$DIST_DIR"

echo "==> Building $PACKAGE_ARCH macOS app..."
BUILD_UNIVERSAL="$BUILD_UNIVERSAL" \
CONFIGURATION="$CONFIGURATION" \
APP_SIGN_IDENTITY="${APP_SIGN_IDENTITY:-}" \
"$ROOT/scripts/build-macos.sh"

if [[ ! -d "$APP_PATH" ]]; then
    echo "error: app bundle not found at $APP_PATH" >&2
    exit 1
fi

echo "==> Preparing installer resources..."
cp "$ROOT/LICENSE" "$RESOURCES_DIR/LICENSE.txt"
mkdir -p "$STAGING_ROOT/Applications"
ditto --norsrc --noextattr --noqtn --noacl "$APP_PATH" "$STAGED_APP"
chmod -R u+rwX "$STAGING_ROOT"
xattr -cr "$STAGING_ROOT" 2>/dev/null || true
find "$STAGING_ROOT" \( -name ".DS_Store" -o -name "._*" -o -name ".__*" \) -delete

cat > "$DISTRIBUTION_XML" <<EOF
<?xml version="1.0" encoding="utf-8"?>
<installer-gui-script minSpecVersion="1">
    <title>$APP_NAME</title>
    <license file="LICENSE.txt" mime-type="text/plain"/>
    <options customize="never" require-scripts="false" rootVolumeOnly="true"/>
    <domains enable_anywhere="false" enable_currentUserHome="false" enable_localSystem="true"/>
    <choices-outline>
        <line choice="default"/>
    </choices-outline>
    <choice id="default" title="$APP_NAME">
        <pkg-ref id="$BUNDLE_ID"/>
    </choice>
    <pkg-ref id="$BUNDLE_ID" version="$VERSION" onConclusion="none">$APP_NAME-component.pkg</pkg-ref>
</installer-gui-script>
EOF

echo "==> Building component package..."
pkgbuild \
    --root "$STAGING_ROOT" \
    --install-location / \
    --identifier "$BUNDLE_ID" \
    --version "$VERSION" \
    --ownership recommended \
    "$COMPONENT_PKG" \
    2> >(grep -v '^write: Permission denied$' >&2)

productbuild_args=(
    --distribution "$DISTRIBUTION_XML"
    --resources "$RESOURCES_DIR"
    --package-path "$WORK_DIR"
)

if [[ -n "${INSTALLER_SIGN_IDENTITY:-}" ]]; then
    productbuild_args+=(--sign "$INSTALLER_SIGN_IDENTITY")
fi

echo "==> Building product package..."
rm -f "$FINAL_PKG"
productbuild "${productbuild_args[@]}" "$FINAL_PKG"

if [[ "${NOTARIZE:-0}" == "1" ]]; then
    if [[ -z "${INSTALLER_SIGN_IDENTITY:-}" ]]; then
        echo "error: notarization requires INSTALLER_SIGN_IDENTITY" >&2
        exit 1
    fi

    echo "==> Submitting package for notarization..."
    if [[ -n "${NOTARY_KEYCHAIN_PROFILE:-}" ]]; then
        xcrun notarytool submit "$FINAL_PKG" \
            --keychain-profile "$NOTARY_KEYCHAIN_PROFILE" \
            --wait
    else
        xcrun notarytool submit "$FINAL_PKG" \
            --apple-id "${APPLE_ID:?APPLE_ID is required for notarization}" \
            --team-id "${APPLE_TEAM_ID:?APPLE_TEAM_ID is required for notarization}" \
            --password "${APPLE_APP_SPECIFIC_PASSWORD:?APPLE_APP_SPECIFIC_PASSWORD is required for notarization}" \
            --wait
    fi

    echo "==> Stapling notarization ticket..."
    xcrun stapler staple "$FINAL_PKG"
fi

echo "==> Verifying package..."
pkgutil --check-signature "$FINAL_PKG" || true
if [[ -n "${INSTALLER_SIGN_IDENTITY:-}" ]]; then
    spctl -a -vv -t install "$FINAL_PKG"
fi

shasum -a 256 "$FINAL_PKG" > "$FINAL_PKG.sha256"

echo "==> Done: $FINAL_PKG"
echo "==> SHA-256: $(cut -d ' ' -f 1 "$FINAL_PKG.sha256")"
