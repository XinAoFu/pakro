#!/usr/bin/env bash
# User-terminal runner. This intentionally does not claim automatic tree cleanup.
set -euo pipefail
if [[ ! -t 0 || ! -t 1 ]]; then
  printf 'Run this script in your own interactive terminal; automatic descendant supervision is unavailable.\n' >&2
  exit 2
fi
p1_manual_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
cd "$p1_manual_root"
p1_manual_mode="${1:-all}"
case "$p1_manual_mode" in build|exact|shared|all) ;; *) printf 'Usage: %s [build|exact|shared|all]\n' "$0" >&2; exit 2;; esac
export CARGO_TARGET_DIR="$p1_manual_root/target"
export CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
export RUST_BACKTRACE=0
mkdir -p "$CARGO_TARGET_DIR"
p1_manual_log="$CARGO_TARGET_DIR/p1-recheck-manual-${p1_manual_mode}-$(date '+%Y%m%d-%H%M%S').log"
p1_manual_tmp="$(mktemp -d /private/tmp/pakro-p1-manual.XXXXXX)"
export TMPDIR="$p1_manual_tmp"
p1_manual_finish() {
  local result=$?
  printf '\nP1_MANUAL_RESULT mode=%s exit=%s automatic_tree_supervision=false\n' "$p1_manual_mode" "$result"
  if [[ "$result" == 0 ]]; then rm -rf "$p1_manual_tmp";
  else printf 'Owned test root retained: %s\n' "$p1_manual_tmp"; fi
  printf 'Complete output: %s\n' "$p1_manual_log"
}
trap p1_manual_finish EXIT
exec > >(tee "$p1_manual_log") 2>&1
printf 'Manual terminal execution: %s. No automatic descendant timeout guarantee.\n' "$p1_manual_mode"
if [[ "$p1_manual_mode" == build || "$p1_manual_mode" == all ]]; then
  cargo build -p pakro --bin pakro --all-features
fi
if [[ "$p1_manual_mode" == exact || "$p1_manual_mode" == all ]]; then
  : "${PAKRO_TEST_PYTHON_ARCHIVE:?set the verified original Python 3.13.16 stripped archive}"
  tests/e2e/run_test tests/e2e/cli/test_p1_exact_python_install
fi
if [[ "$p1_manual_mode" == shared || "$p1_manual_mode" == all ]]; then
  : "${PAKRO_TEST_PYTHON_ARCHIVE:?set the verified original Python 3.13.16 stripped archive}"
  tests/e2e/run_test tests/e2e/packages/test_native_install_shared_wheel
fi
