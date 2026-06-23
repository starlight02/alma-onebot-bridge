#!/bin/bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
source "$ROOT/scripts/version-lib.sh"

EXPECTED_TAG=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --tag)
            if [[ $# -lt 2 ]]; then
                echo "error: --tag requires a value" >&2
                exit 1
            fi
            EXPECTED_TAG="$2"
            shift 2
            ;;
        --tag=*)
            EXPECTED_TAG="${1#--tag=}"
            shift
            ;;
        *)
            echo "usage: $0 [--tag vX.Y.Z]" >&2
            exit 1
            ;;
    esac
done

verify_repo_versions "$ROOT" "$EXPECTED_TAG"
