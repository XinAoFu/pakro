#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/release-env.sh"
pakro_require_repository
gh release view -R "$PAKRO_RELEASE_REPOSITORY" --json tagName | jq -r '.tagName'
