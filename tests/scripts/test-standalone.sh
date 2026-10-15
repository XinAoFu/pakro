#!/usr/bin/env bash
set -euxo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../.."
python3 tests/scripts/test-install-release-age.py

# This integration check requires a configured pakro release host.
PAKRO_DOWNLOAD_BASE_URL="${PAKRO_DOWNLOAD_BASE_URL:-${PAKRO_RELEASE_URL:-}}"
: "${PAKRO_DOWNLOAD_BASE_URL:?Set PAKRO_DOWNLOAD_BASE_URL to the pakro release host}"
PAKRO_DOWNLOAD_BASE_URL="${PAKRO_DOWNLOAD_BASE_URL%/}"
BASE_DIR="$(pwd)"
RELEASE_DIR="$(mktemp -d)"
trap 'rm -rf "$RELEASE_DIR"' EXIT
PAKRO_VERSION="v$(curl -fsSL --retry 3 --retry-all-errors --retry-delay 2 "$PAKRO_DOWNLOAD_BASE_URL/VERSION")"
export BASE_DIR RELEASE_DIR PAKRO_VERSION PAKRO_DOWNLOAD_BASE_URL

mkdir -p "$RELEASE_DIR/$PAKRO_VERSION"
curl -fsSL --retry 3 --retry-all-errors --retry-delay 2 "$PAKRO_DOWNLOAD_BASE_URL/$PAKRO_VERSION/SHASUMS256.txt" >"$RELEASE_DIR/$PAKRO_VERSION/SHASUMS256.txt"
./packaging/scripts/render-install.sh >"$RELEASE_DIR/install.sh"
chmod +x "$RELEASE_DIR/install.sh"
pakro x shellcheck -- shellcheck "$RELEASE_DIR/install.sh"

PAKRO_INSTALL_PATH="$RELEASE_DIR/pakro" "$RELEASE_DIR/install.sh"
if [[ ! "$("$RELEASE_DIR/pakro" -v)" =~ ^${PAKRO_VERSION//v/} ]]; then
  echo "pakro version mismatch"
  exit 1
fi
