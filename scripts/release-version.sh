#!/bin/bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
source "$ROOT/scripts/version-lib.sh"

usage() {
    echo "usage: $0 X.Y.Z" >&2
}

if [[ $# -ne 1 ]]; then
    usage
    exit 1
fi

VERSION="$1"
validate_release_version "$VERSION"
BUILD_NUMBER="$(build_number_for_version "$VERSION")"
TAG="v$VERSION"
export VERSION BUILD_NUMBER

if git -C "$ROOT" rev-parse -q --verify "refs/tags/$TAG" >/dev/null; then
    echo "error: tag already exists locally: $TAG" >&2
    exit 1
fi

perl -0pi -e 's/\A(\[package\]\n(?:[^\n]*\n)*?version = ")[^"]+(")/$1$ENV{VERSION}$2/s' \
    "$ROOT/Cargo.toml"

perl -0pi -e 's/(\[\[package\]\]\nname = "alma-onebot-bridge"\nversion = ")[^"]+(")/$1$ENV{VERSION}$2/s' \
    "$ROOT/Cargo.lock"
cargo metadata --locked --no-deps --format-version 1 --manifest-path "$ROOT/Cargo.toml" >/dev/null

perl -0pi -e 's/(MARKETING_VERSION = )[^;]+(;)/$1$ENV{VERSION}$2/g' \
    "$ROOT/platforms/macos/AlmaOneBotBridge.xcodeproj/project.pbxproj"
perl -0pi -e 's/(CURRENT_PROJECT_VERSION = )[^;]+(;)/$1$ENV{BUILD_NUMBER}$2/g' \
    "$ROOT/platforms/macos/AlmaOneBotBridge.xcodeproj/project.pbxproj"

"$ROOT/scripts/verify-version.sh"

cat <<EOF
Updated release version to $VERSION.

Next:
  git diff -- Cargo.toml Cargo.lock platforms/macos/AlmaOneBotBridge.xcodeproj/project.pbxproj
  include these version changes in the release commit
  git tag -a "$TAG" -m "$TAG"
EOF
