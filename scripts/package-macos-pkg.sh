#!/bin/bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP_NAME="AlmaOneBotBridge"
BUNDLE_ID="moe.aili.alma-onebot-bridge"
VERSION="${VERSION:-$(awk -F\" '/^version =/ {print $2; exit}' "$ROOT/Cargo.toml")}"
IFS=. read -r VERSION_MAJOR VERSION_MINOR VERSION_PATCH _ <<< "$VERSION"
VERSION_MAJOR="${VERSION_MAJOR:-0}"
VERSION_MINOR="${VERSION_MINOR:-0}"
VERSION_PATCH="${VERSION_PATCH:-0}"
BUILD_NUMBER="${BUILD_NUMBER:-$((10#$VERSION_MAJOR * 10000 + 10#$VERSION_MINOR * 100 + 10#$VERSION_PATCH))}"
CONFIGURATION="${CONFIGURATION:-Release}"
DIST_DIR="${DIST_DIR:-$ROOT/dist/macos}"
BUILD_UNIVERSAL="${BUILD_UNIVERSAL:-1}"
PACKAGE_ARCH="${PACKAGE_ARCH:-}"
WORK_DIR="$DIST_DIR/work"
RESOURCES_DIR="$WORK_DIR/resources"
DISTRIBUTION_XML="$WORK_DIR/Distribution.xml"
COMPONENT_PLIST="$WORK_DIR/components.plist"
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
VERSION="$VERSION" \
BUILD_NUMBER="$BUILD_NUMBER" \
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

cat > "$COMPONENT_PLIST" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<array>
    <dict>
        <key>BundleHasStrictIdentifier</key>
        <true/>
        <key>BundleIsRelocatable</key>
        <false/>
        <key>BundleIsVersionChecked</key>
        <false/>
        <key>BundleOverwriteAction</key>
        <string>upgrade</string>
        <key>RootRelativeBundlePath</key>
        <string>Applications/$APP_NAME.app</string>
    </dict>
</array>
</plist>
EOF

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
    --component-plist "$COMPONENT_PLIST" \
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

echo "==> Inspecting installer payload..."
PACKAGE_CHECK_DIR="$WORK_DIR/check"
rm -rf "$PACKAGE_CHECK_DIR"
pkgutil --expand-full "$FINAL_PKG" "$PACKAGE_CHECK_DIR"
COMPONENT_INFO="$PACKAGE_CHECK_DIR/$APP_NAME-component.pkg/PackageInfo"
PACKAGE_APP="$PACKAGE_CHECK_DIR/$APP_NAME-component.pkg/Payload/Applications/$APP_NAME.app"
PACKAGE_INFO_PLIST="$PACKAGE_APP/Contents/Info.plist"
PACKAGE_ICONSET_CHECK="$PACKAGE_CHECK_DIR/AppIcon.iconset"
if ! grep -q 'install-location="/"' "$COMPONENT_INFO"; then
    echo "error: component package root install location changed" >&2
    exit 1
fi
if ! grep -q "path=\"./Applications/$APP_NAME.app\"" "$COMPONENT_INFO"; then
    echo "error: component package payload does not contain Applications/$APP_NAME.app" >&2
    exit 1
fi
if grep -q '<bundle-version>' "$COMPONENT_INFO"; then
    echo "error: component package still enables bundle version checks" >&2
    exit 1
fi
if [[ ! -f "$PACKAGE_APP/Contents/Resources/AppIcon.icns" ]]; then
    echo "error: package payload is missing AppIcon.icns" >&2
    exit 1
fi
iconutil -c iconset "$PACKAGE_APP/Contents/Resources/AppIcon.icns" -o "$PACKAGE_ICONSET_CHECK"
if [[ ! -f "$PACKAGE_ICONSET_CHECK/icon_512x512@2x.png" ]]; then
    echo "error: package AppIcon.icns is missing the 1024px rendition" >&2
    exit 1
fi
if [[ "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIconFile' "$PACKAGE_INFO_PLIST")" != "AppIcon.icns" ]]; then
    echo "error: package Info.plist is missing CFBundleIconFile=AppIcon.icns" >&2
    exit 1
fi
if /usr/libexec/PlistBuddy -c 'Print :CFBundleIconName' "$PACKAGE_INFO_PLIST" >/dev/null 2>&1; then
    echo "error: package Info.plist must not use asset-catalog CFBundleIconName" >&2
    exit 1
fi
if [[ "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$PACKAGE_INFO_PLIST")" != "$VERSION" ]]; then
    echo "error: package Info.plist version does not match $VERSION" >&2
    exit 1
fi
if [[ "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "$PACKAGE_INFO_PLIST")" != "$BUILD_NUMBER" ]]; then
    echo "error: package Info.plist build number does not match $BUILD_NUMBER" >&2
    exit 1
fi

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
