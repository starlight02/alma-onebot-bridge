#!/bin/bash

read_cargo_version() {
    local root="$1"
    awk -F\" '/^version =/ {print $2; exit}' "$root/Cargo.toml"
}

validate_release_version() {
    local version="$1"
    if [[ ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
        echo "error: version must use major.minor.patch, got: $version" >&2
        return 1
    fi
}

build_number_for_version() {
    local version="$1"
    local major minor patch extra
    IFS=. read -r major minor patch extra <<< "$version"
    major="${major:-0}"
    minor="${minor:-0}"
    patch="${patch:-0}"
    echo $((10#$major * 10000 + 10#$minor * 100 + 10#$patch))
}

read_lockfile_package_version() {
    local root="$1"
    awk '
        /^\[\[package\]\]/ {
            in_package = 1
            is_target = 0
            next
        }
        in_package && /^name = "alma-onebot-bridge"/ {
            is_target = 1
            next
        }
        in_package && is_target && /^version = / {
            gsub(/"/, "", $3)
            print $3
            exit
        }
    ' "$root/Cargo.lock"
}

read_xcode_build_setting_values() {
    local root="$1"
    local setting="$2"
    sed -n "s/^[[:space:]]*$setting = \\([^;]*\\);/\\1/p" \
        "$root/platforms/macos/AlmaOneBotBridge.xcodeproj/project.pbxproj" |
        sed 's/^"//; s/"$//'
}

verify_repo_versions() {
    local root="$1"
    local expected_tag="${2:-}"
    local version build_number lock_version value mismatch seen_marketing seen_build

    version="$(read_cargo_version "$root")"
    validate_release_version "$version"
    build_number="$(build_number_for_version "$version")"

    lock_version="$(read_lockfile_package_version "$root")"
    if [[ "$lock_version" != "$version" ]]; then
        echo "error: Cargo.lock alma-onebot-bridge version is $lock_version, expected $version" >&2
        return 1
    fi

    mismatch=0
    seen_marketing=0
    while IFS= read -r value; do
        seen_marketing=1
        if [[ "$value" != "$version" ]]; then
            echo "error: Xcode MARKETING_VERSION is $value, expected $version" >&2
            mismatch=1
        fi
    done < <(read_xcode_build_setting_values "$root" "MARKETING_VERSION")
    if [[ "$seen_marketing" -eq 0 ]]; then
        echo "error: Xcode MARKETING_VERSION is missing" >&2
        mismatch=1
    fi

    seen_build=0
    while IFS= read -r value; do
        seen_build=1
        if [[ "$value" != "$build_number" ]]; then
            echo "error: Xcode CURRENT_PROJECT_VERSION is $value, expected $build_number" >&2
            mismatch=1
        fi
    done < <(read_xcode_build_setting_values "$root" "CURRENT_PROJECT_VERSION")
    if [[ "$seen_build" -eq 0 ]]; then
        echo "error: Xcode CURRENT_PROJECT_VERSION is missing" >&2
        mismatch=1
    fi

    if [[ "$mismatch" -ne 0 ]]; then
        return 1
    fi

    if [[ -n "$expected_tag" && "$expected_tag" != "v$version" ]]; then
        echo "error: release tag $expected_tag does not match Cargo.toml version v$version" >&2
        return 1
    fi

    echo "version ok: $version (build $build_number)"
}
