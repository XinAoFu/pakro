#!/usr/bin/env bash
# User-terminal execution. Automatic multiprocess supervision remains disabled.
set -euo pipefail
if [[ ! -t 0 || ! -t 1 ]]; then
  printf 'Run this script in your own interactive terminal; automatic descendant supervision is unavailable.\n' >&2
  exit 2
fi
p2_manual_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
cd "$p2_manual_root"
p2_manual_mode="${1:-unit}"
case "$p2_manual_mode" in build|unit|wheel|source|source-rebuild|regression|list) ;; *) printf 'Usage: %s [build|unit|wheel|source|source-rebuild|regression|list]\n' "$0" >&2; exit 2;; esac
export CARGO_TARGET_DIR="$p2_manual_root/target"
export CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
export RUST_BACKTRACE=0
mkdir -p "$CARGO_TARGET_DIR"
p2_manual_log="$CARGO_TARGET_DIR/p2-manual-${p2_manual_mode}-$(date '+%Y%m%d-%H%M%S').log"
p2_manual_tmp="$(mktemp -d /private/tmp/pakro-p2-manual.XXXXXX)"
export TMPDIR="$p2_manual_tmp"
p2_manual_finish() {
  local result=$?
  printf '\nP2_MANUAL_RESULT mode=%s exit=%s automatic_tree_supervision=false\n' "$p2_manual_mode" "$result"
  if [[ "$result" == 0 ]]; then rm -rf "$p2_manual_tmp";
  else printf 'Owned test root retained: %s\n' "$p2_manual_tmp"; fi
  printf 'Complete output: %s\n' "$p2_manual_log"
}
trap p2_manual_finish EXIT
exec > >(tee "$p2_manual_log") 2>&1
printf 'P2 manual terminal group: %s. No automatic descendant timeout guarantee.\n' "$p2_manual_mode"
p2_require_passed_tests() {
  python3 - "$1" "$2" <<'PY'
import re, sys
from pathlib import Path
results = re.findall(r'^test result: ok\. (\d+) passed; 0 failed;', Path(sys.argv[1]).read_text(), re.MULTILINE)
minimum = int(sys.argv[2])
if not any(int(count) >= minimum for count in results):
    raise SystemExit(f'no successful nonzero test result reached the required {minimum} cases')
print(f'P2_RESULT_VERIFIED required_passed={minimum} observed_summaries={results}')
PY
}
if [[ "$p2_manual_mode" == list ]]; then
  cat tests/scripts/p2-test-cases.txt
elif [[ "$p2_manual_mode" == build ]]; then
  cargo build -p pakro --bin pakro --all-features
  cargo test -p pakro --lib --all-features --no-run
  cargo test -p pakro --bin pakro --all-features --no-run
elif [[ "$p2_manual_mode" == unit ]]; then
  : "${PAKRO_TEST_PYTHON_ARCHIVE:?set the verified original Python 3.13.16 stripped archive}"
  p2_registered="$CARGO_TARGET_DIR/p2-registered-tests.txt"
  cargo test -p pakro --lib --all-features p2_ -- --list >"$p2_registered" 2>&1
  cat "$p2_registered"
  p2_count=0
  while read -r p2_package p2_target p2_case; do
    [[ "$p2_package" == pakro && "$p2_target" == lib ]] || continue
    grep -Fxq "$p2_case: test" "$p2_registered" || { printf 'Missing P2 test: %s\n' "$p2_case" >&2; exit 1; }
    p2_count=$((p2_count + 1))
  done <tests/scripts/p2-test-cases.txt
  [[ "$p2_count" -gt 0 ]] || { printf 'No P2 unit tests registered\n' >&2; exit 1; }
  p2_results="$CARGO_TARGET_DIR/p2-unit-results.txt"
  cargo test -p pakro --lib --all-features p2_ -- --nocapture | tee "$p2_results"
  p2_require_passed_tests "$p2_results" "$p2_count"
  # Existing replay closure, real metadata, targets, deadlines and pin failure
  # contracts are directly affected by the newly supported source branch.
  p2_replay_filter='packages::python_resolution::source::locked_graph::tests::'
  p2_replay_registered="$CARGO_TARGET_DIR/p2-replay-registered-tests.txt"
  cargo test -p pakro --lib --all-features "$p2_replay_filter" -- --list >"$p2_replay_registered" 2>&1
  cat "$p2_replay_registered"
  p2_replay_count="$(grep -c ': test$' "$p2_replay_registered")"
  [[ "$p2_replay_count" -gt 0 ]] || { printf 'No strict replay tests registered\n' >&2; exit 1; }
  p2_replay_results="$CARGO_TARGET_DIR/p2-replay-results.txt"
  cargo test -p pakro --lib --all-features "$p2_replay_filter" -- --nocapture | tee "$p2_replay_results"
  p2_require_passed_tests "$p2_replay_results" "$p2_replay_count"
  p2_facade_test='programs::python::integration_tests::f3_project_facade_reuses_real_closure_and_preserves_failure'
  p2_facade_registered="$CARGO_TARGET_DIR/p2-facade-registered-tests.txt"
  cargo test -p pakro --lib --all-features "$p2_facade_test" -- --exact --list >"$p2_facade_registered" 2>&1
  cat "$p2_facade_registered"
  grep -Fxq "$p2_facade_test: test" "$p2_facade_registered" || { printf 'Missing real project facade regression\n' >&2; exit 1; }
  p2_facade_results="$CARGO_TARGET_DIR/p2-facade-results.txt"
  cargo test -p pakro --lib --all-features "$p2_facade_test" -- --exact --nocapture | tee "$p2_facade_results"
  p2_require_passed_tests "$p2_facade_results" 1
  # Publication now returns its actual committed file receipt. Exercise the
  # existing replacement/dangling kernels that share this changed primitive.
  for p2_io_filter in 'lockfile::transaction::tests::' 'lockfile::transaction::dangling::tests::'; do
    p2_io_key="${p2_io_filter//:/_}"
    p2_io_registered="$CARGO_TARGET_DIR/p2-io-${p2_io_key}-registered.txt"
    cargo test -p pakro --lib --all-features "$p2_io_filter" -- --list >"$p2_io_registered" 2>&1
    cat "$p2_io_registered"
    p2_io_count="$(grep -c ': test$' "$p2_io_registered")"
    [[ "$p2_io_count" -gt 0 ]] || { printf 'No lock IO tests registered: %s\n' "$p2_io_filter" >&2; exit 1; }
    p2_io_results="$CARGO_TARGET_DIR/p2-io-${p2_io_key}-results.txt"
    cargo test -p pakro --lib --all-features "$p2_io_filter" -- --nocapture | tee "$p2_io_results"
    p2_require_passed_tests "$p2_io_results" "$p2_io_count"
  done
elif [[ "$p2_manual_mode" == source-rebuild ]]; then
  : "${PAKRO_TEST_PYTHON_ARCHIVE:?set the verified original Python 3.13.16 stripped archive}"
  p2_source_rebuild_test='programs::install::build::tests::p2_source_cold_and_warm_replay_keeps_recursive_inputs_and_rejects_changed_output'
  p2_source_rebuild_registered="$CARGO_TARGET_DIR/p2-source-rebuild-registered.txt"
  cargo test -p pakro --lib --all-features "$p2_source_rebuild_test" -- --exact --list >"$p2_source_rebuild_registered" 2>&1
  cat "$p2_source_rebuild_registered"
  grep -Fxq "$p2_source_rebuild_test: test" "$p2_source_rebuild_registered" || { printf 'Missing exact source rebuild case\n' >&2; exit 1; }
  p2_source_rebuild_results="$CARGO_TARGET_DIR/p2-source-rebuild-results.txt"
  cargo test -p pakro --lib --all-features "$p2_source_rebuild_test" -- --exact --nocapture | tee "$p2_source_rebuild_results"
  p2_require_passed_tests "$p2_source_rebuild_results" 1
elif [[ "$p2_manual_mode" == wheel || "$p2_manual_mode" == source ]]; then
  : "${PAKRO_TEST_PYTHON_ARCHIVE:?set the verified original Python 3.13.16 stripped archive}"
  [[ -x "$CARGO_TARGET_DIR/debug/pakro" ]] || { printf 'Run the build group first\n' >&2; exit 1; }
  if [[ "$p2_manual_mode" == wheel ]]; then
    tests/e2e/run_test tests/e2e/packages/test_p2_strict_lock_replay
  else
    tests/e2e/run_test tests/e2e/packages/test_p2_source_strict_lock_replay
  fi
else
  tests/e2e/run_test tests/e2e/cli/test_p2_pending_preflight_is_readonly
  : "${PAKRO_TEST_PYTHON_ARCHIVE:?set the verified original Python 3.13.16 stripped archive}"
  tests/e2e/run_test tests/e2e/packages/test_native_install_shared_wheel
  tests/e2e/run_test tests/e2e/packages/test_native_deps_install_preserves_declarations
fi
