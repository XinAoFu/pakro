#!/usr/bin/env bash

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" &>/dev/null && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

resolve_pakro_bin() {
  if [[ -n ${PAKRO_E2E_BIN:-} ]]; then
    printf '%s\n' "$PAKRO_E2E_BIN"
    return
  fi
  local candidate="${CARGO_TARGET_DIR:-$ROOT/target}/debug/pakro"
  if [[ -x $candidate ]]; then
    printf '%s\n' "$candidate"
    return
  fi
  local pakro_path
  if ! pakro_path="$(command -v pakro 2>/dev/null)"; then
    err "Could not find 'pakro' in PATH and no local debug build was found. Set PAKRO_E2E_BIN or build/install pakro."
    return 1
  fi
  printf '%s\n' "$pakro_path"
}
