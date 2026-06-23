#!/bin/bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
source "$ROOT/scripts/version-lib.sh"
APP_NAME="AlmaOneBotBridge"
BUNDLE_ID="moe.aili.alma-onebot-bridge"
VERSION_FROM_ENV="${VERSION+x}"
VERSION="${VERSION:-$(read_cargo_version "$ROOT")}"
validate_release_version "$VERSION"
BUILD_NUMBER="${BUILD_NUMBER:-$(build_number_for_version "$VERSION")}"
CONFIGURATION="${CONFIGURATION:-Release}"
DIST_DIR="${DIST_DIR:-$ROOT/dist/macos}"
BUILD_UNIVERSAL="${BUILD_UNIVERSAL:-1}"
PACKAGE_ARCH="${PACKAGE_ARCH:-}"
WORK_DIR="$DIST_DIR/work"
RESOURCES_DIR="$WORK_DIR/resources"
SCRIPTS_DIR="$WORK_DIR/scripts"
DISTRIBUTION_XML="$WORK_DIR/Distribution.xml"
COMPONENT_PLIST="$WORK_DIR/components.plist"
APP_PATH="$ROOT/platforms/macos/build/Build/Products/$CONFIGURATION/$APP_NAME.app"
STAGING_ROOT="$WORK_DIR/root"
STAGED_APP="$STAGING_ROOT/Applications/$APP_NAME.app"
COMPONENT_PKG="$WORK_DIR/$APP_NAME-component.pkg"

export COPYFILE_DISABLE=1

if [[ -z "$VERSION_FROM_ENV" && "${SKIP_VERSION_VERIFY:-0}" != "1" ]]; then
    "$ROOT/scripts/verify-version.sh"
fi

if [[ -z "${GIT_COMMIT:-}" || -z "${GIT_VERSION:-}" || -z "${GIT_DIRTY:-}" ]]; then
    if git -C "$ROOT" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
        RESOLVED_GIT_COMMIT="$(git -C "$ROOT" rev-parse --short=12 HEAD)"
        RESOLVED_GIT_VERSION="$(git -C "$ROOT" describe --tags --always 2>/dev/null || printf '%s' "$RESOLVED_GIT_COMMIT")"
        RESOLVED_GIT_DIRTY=false
        if ! git -C "$ROOT" diff --quiet --ignore-submodules -- \
            || ! git -C "$ROOT" diff --cached --quiet --ignore-submodules --; then
            RESOLVED_GIT_DIRTY=true
            RESOLVED_GIT_VERSION="$RESOLVED_GIT_VERSION-dirty"
        fi
    else
        RESOLVED_GIT_COMMIT=unknown
        RESOLVED_GIT_VERSION=unknown
        RESOLVED_GIT_DIRTY=false
    fi

    GIT_COMMIT="${GIT_COMMIT:-$RESOLVED_GIT_COMMIT}"
    GIT_VERSION="${GIT_VERSION:-$RESOLVED_GIT_VERSION}"
    GIT_DIRTY="${GIT_DIRTY:-$RESOLVED_GIT_DIRTY}"
fi

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
mkdir -p "$RESOURCES_DIR" "$SCRIPTS_DIR" "$DIST_DIR"

echo "==> Building $PACKAGE_ARCH macOS app..."
BUILD_UNIVERSAL="$BUILD_UNIVERSAL" \
CONFIGURATION="$CONFIGURATION" \
VERSION="$VERSION" \
BUILD_NUMBER="$BUILD_NUMBER" \
GIT_COMMIT="$GIT_COMMIT" \
GIT_VERSION="$GIT_VERSION" \
GIT_DIRTY="$GIT_DIRTY" \
SKIP_VERSION_VERIFY=1 \
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

cat > "$SCRIPTS_DIR/preinstall" <<EOF
#!/bin/sh
set -u

APP_NAME="$APP_NAME"
BUNDLE_ID="$BUNDLE_ID"
APP_EXEC="/Applications/\$APP_NAME.app/Contents/MacOS/\$APP_NAME"
BRIDGE_EXEC="/Applications/\$APP_NAME.app/Contents/Resources/alma-onebot-bridge"
MARKER_FILE="/private/tmp/\$BUNDLE_ID.was-running"

matching_pids() {
    executable_prefix="\$1"
    /bin/ps -axo pid=,command= | /usr/bin/awk -v prefix="\$executable_prefix" '
        {
            pid = \$1
            sub(/^[[:space:]]*[0-9]+[[:space:]]+/, "", \$0)
            if (index(\$0, prefix) == 1) {
                print pid
            }
        }
    '
}

running_pids() {
    {
        matching_pids "\$APP_EXEC"
        matching_pids "\$BRIDGE_EXEC"
    } | /usr/bin/sort -u
}

has_running_processes() {
    [ -n "\$(running_pids)" ]
}

console_user() {
    /usr/bin/stat -f '%Su' /dev/console 2>/dev/null || true
}

console_uid() {
    user="\$1"
    [ -n "\$user" ] && [ "\$user" != "root" ] && [ "\$user" != "loginwindow" ] || return 1
    /usr/bin/id -u "\$user" 2>/dev/null
}

run_as_console_user() {
    user="\$(console_user)"
    uid="\$(console_uid "\$user")" || return 1
    /bin/launchctl asuser "\$uid" "\$@"
}

request_app_quit() {
    if [ -z "\$(matching_pids "\$APP_EXEC")" ]; then
        return 0
    fi

    echo "Requesting \$APP_NAME to quit before installing..."
    if ! run_as_console_user /usr/bin/osascript -e "tell application id \"\$BUNDLE_ID\" to quit"; then
        /usr/bin/osascript -e "tell application id \"\$BUNDLE_ID\" to quit" >/dev/null 2>&1 || true
    fi
}

signal_running_processes() {
    signal="\$1"
    running_pids | while read -r pid; do
        [ -n "\$pid" ] || continue
        /bin/kill "-\$signal" "\$pid" 2>/dev/null || true
    done
}

wait_until_stopped() {
    attempts="\$1"
    index=0
    while [ "\$index" -lt "\$attempts" ]; do
        if ! has_running_processes; then
            return 0
        fi
        /bin/sleep 0.2
        index=\$((index + 1))
    done
    return 1
}

if ! has_running_processes; then
    /bin/rm -f "\$MARKER_FILE"
    exit 0
fi

user="\$(console_user)"
uid="\$(console_uid "\$user" 2>/dev/null || true)"
{
    printf 'user=%s\n' "\$user"
    printf 'uid=%s\n' "\$uid"
} > "\$MARKER_FILE"

request_app_quit
if wait_until_stopped 60; then
    echo "\$APP_NAME stopped cleanly."
    exit 0
fi

echo "\$APP_NAME did not quit in time; sending SIGTERM to installed app processes..."
signal_running_processes TERM
if wait_until_stopped 30; then
    exit 0
fi

echo "\$APP_NAME still running; sending SIGKILL to installed app processes..."
signal_running_processes KILL
if wait_until_stopped 10; then
    exit 0
fi

echo "error: unable to stop \$APP_NAME before installation." >&2
exit 1
EOF

cat > "$SCRIPTS_DIR/postinstall" <<EOF
#!/bin/sh
set -u

APP_NAME="$APP_NAME"
BUNDLE_ID="$BUNDLE_ID"
MARKER_FILE="/private/tmp/\$BUNDLE_ID.was-running"

if [ ! -f "\$MARKER_FILE" ]; then
    exit 0
fi

uid=""
while IFS='=' read -r key value; do
    case "\$key" in
        uid) uid="\$value" ;;
    esac
done < "\$MARKER_FILE"
/bin/rm -f "\$MARKER_FILE"

if [ -n "\$uid" ]; then
    /bin/launchctl asuser "\$uid" /usr/bin/open "/Applications/\$APP_NAME.app" >/dev/null 2>&1 \
        || /bin/launchctl asuser "\$uid" /usr/bin/open -b "\$BUNDLE_ID" >/dev/null 2>&1 \
        || true
else
    /usr/bin/open "/Applications/\$APP_NAME.app" >/dev/null 2>&1 || true
fi

exit 0
EOF
chmod 755 "$SCRIPTS_DIR/preinstall" "$SCRIPTS_DIR/postinstall"

echo "==> Building component package..."
pkgbuild \
    --root "$STAGING_ROOT" \
    --scripts "$SCRIPTS_DIR" \
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
PACKAGE_SCRIPTS="$PACKAGE_CHECK_DIR/$APP_NAME-component.pkg/Scripts"
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
if [[ ! -x "$PACKAGE_SCRIPTS/preinstall" ]]; then
    echo "error: component package is missing executable preinstall script" >&2
    exit 1
fi
if [[ ! -x "$PACKAGE_SCRIPTS/postinstall" ]]; then
    echo "error: component package is missing executable postinstall script" >&2
    exit 1
fi
if ! grep -q "$BUNDLE_ID" "$PACKAGE_SCRIPTS/preinstall"; then
    echo "error: preinstall script does not target $BUNDLE_ID" >&2
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
if [[ "$(/usr/libexec/PlistBuddy -c 'Print :AlmaGitCommit' "$PACKAGE_INFO_PLIST")" != "$GIT_COMMIT" ]]; then
    echo "error: package Info.plist git commit does not match $GIT_COMMIT" >&2
    exit 1
fi
if [[ "$(/usr/libexec/PlistBuddy -c 'Print :AlmaGitVersion' "$PACKAGE_INFO_PLIST")" != "$GIT_VERSION" ]]; then
    echo "error: package Info.plist git version does not match $GIT_VERSION" >&2
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
