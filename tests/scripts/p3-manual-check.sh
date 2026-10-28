#!/usr/bin/env bash
# User-terminal execution only; no automatic descendant supervisor is enabled.
set -euo pipefail
if [[ ! -t 0 || ! -t 1 ]]; then
  printf 'Run this script in your own interactive terminal; automatic descendant supervision is unavailable.\n' >&2
  exit 2
fi
p3_manual_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
cd "$p3_manual_root"
p3_manual_mode="${1:-unit}"
case "$p3_manual_mode" in build|unit|repair|cache|recovery|retained|source-retained|extras|lifecycle|list) ;; *) printf 'Usage: %s [build|unit|repair|cache|recovery|retained|source-retained|extras|lifecycle|list]\n' "$0" >&2; exit 2;; esac
export CARGO_TARGET_DIR="$p3_manual_root/target"
export CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
export RUST_BACKTRACE=0 PYTHONDONTWRITEBYTECODE=1 RUST_TEST_THREADS=1
mkdir -p "$CARGO_TARGET_DIR"
p3_manual_stamp="$(date '+%Y%m%d-%H%M%S')"
p3_manual_log="$CARGO_TARGET_DIR/p3-manual-${p3_manual_mode}-${p3_manual_stamp}.log"
p3_manual_source="$CARGO_TARGET_DIR/p3-manual-${p3_manual_mode}-${p3_manual_stamp}-source.json"
p3_manual_tmp="$(mktemp -d "${TMPDIR:-/tmp}/pakro-p3-manual.XXXXXX")"
p3_manual_tmp="$(cd "$p3_manual_tmp" && pwd -P)"
export TMPDIR="$p3_manual_tmp"
p3_fixture="$p3_manual_root/tests/e2e/packages/p3_project_fixture.py"
p3_manual_complete=0
p3_manual_finish() {
  local result=$?
  trap - EXIT
  if [[ "$p3_manual_complete" != 1 ]]; then
    printf 'P3_GROUP_COMPLETE=false; the requested steps did not all finish\n'
    if [[ "$result" == 0 ]]; then result=1; fi
  else
    printf 'P3_GROUP_COMPLETE=true\n'
  fi
  if python3 -B "$p3_fixture" source-manifest "$p3_manual_root" >"${p3_manual_source%.json}-after.json" && cmp -s "$p3_manual_source" "${p3_manual_source%.json}-after.json"; then
    printf 'P3_SOURCE_UNCHANGED=true\n'
  else
    printf 'P3_SOURCE_UNCHANGED=false; results do not describe one unchanged source snapshot\n'
    result=1
  fi
  if [[ "$result" == 0 ]]; then
    if ! rm -rf "$p3_manual_tmp"; then
      result=1
      printf 'Owned test root cleanup did not finish: %s\n' "$p3_manual_tmp"
    fi
  else printf 'Owned test root retained: %s\n' "$p3_manual_tmp"; fi
  printf '\nP3_MANUAL_RESULT mode=%s exit=%s automatic_tree_supervision=false elapsed_seconds=%s\n' "$p3_manual_mode" "$result" "$SECONDS"
  printf 'Complete output: %s\nSource manifest: %s\n' "$p3_manual_log" "$p3_manual_source"
  exit "$result"
}
trap p3_manual_finish EXIT
exec > >(tee "$p3_manual_log") 2>&1
printf 'P3 manual terminal group: %s. Stop this group if it reaches 600 seconds; no automatic descendant timeout guarantee.\n' "$p3_manual_mode"
git rev-parse HEAD
rustc --version --verbose
cargo --version
python3 -B -c 'import os, resource; print("P3_RLIMIT_NOFILE=" + str(resource.getrlimit(resource.RLIMIT_NOFILE))); print("P3_RLIMIT_STACK=" + str(resource.getrlimit(resource.RLIMIT_STACK))); print("P3_RUST_MIN_STACK=" + repr(os.environ.get("RUST_MIN_STACK")))'
python3 -B "$p3_fixture" source-manifest "$p3_manual_root" >"$p3_manual_source"
if [[ "$p3_manual_mode" == unit || "$p3_manual_mode" == repair || "$p3_manual_mode" == cache || "$p3_manual_mode" == source-retained || "$p3_manual_mode" == lifecycle || "$p3_manual_mode" == extras ]]; then
  : "${PAKRO_TEST_PYTHON_ARCHIVE:?set the verified original CPython 3.13.16 stripped archive}"
  printf 'PAKRO_TEST_PYTHON_ARCHIVE=%s\n' "$PAKRO_TEST_PYTHON_ARCHIVE"
fi
if [[ "$p3_manual_mode" == lifecycle || "$p3_manual_mode" == extras ]]; then
  : "${PAKRO_TEST_PYTHON_SECOND_ARCHIVE:?set the verified original CPython 3.14.9+20261021 stripped archive}"
  printf 'PAKRO_TEST_PYTHON_SECOND_ARCHIVE=%s\n' "$PAKRO_TEST_PYTHON_SECOND_ARCHIVE"
fi
if [[ "$p3_manual_mode" == list ]]; then
  printf '%s\n' 'cargo test -p pakro --lib --all-features p3_ -- --nocapture' 'cargo test -p pakro --bin pakro --all-features p3_ -- --nocapture (only when nonzero cases are registered)' 'tests/e2e/packages/test_p3_project_lifecycle'
elif [[ "$p3_manual_mode" == build ]]; then
  cargo test -p pakro --lib --all-features --no-run
  cargo test -p pakro --bin pakro --all-features --no-run
  cargo build -p pakro --bin pakro --all-features
elif [[ "$p3_manual_mode" == unit || "$p3_manual_mode" == repair || "$p3_manual_mode" == cache || "$p3_manual_mode" == recovery || "$p3_manual_mode" == retained || "$p3_manual_mode" == source-retained ]]; then
  p3_targets=(lib bin)
  if [[ "$p3_manual_mode" == cache || "$p3_manual_mode" == retained || "$p3_manual_mode" == source-retained ]]; then p3_targets=(lib); fi
  for p3_target in "${p3_targets[@]}"; do
    p3_target_args=("--$p3_target")
    if [[ "$p3_target" == bin ]]; then p3_target_args+=(pakro); fi
    p3_registered="$CARGO_TARGET_DIR/p3-${p3_target}-${p3_manual_stamp}-registered.txt"
    p3_filter='p3_'
    p3_extra_filters=()
    if [[ "$p3_manual_mode" == cache ]]; then
      p3_filter='programs::install::build::tests::p3_strict_cached_source_outputs_reject_changed_backend_and_request_without_publication'
      p3_extra_filters+=(
        'packages::python_resolution::source::locked_graph::tests::p2_source_replay_retains_original_pin_and_exact_cached_output'
        'programs::install::build::tests::p2_source_cold_and_warm_replay_keeps_recursive_inputs_and_rejects_changed_output'
      )
    elif [[ "$p3_manual_mode" == repair && "$p3_target" == lib ]]; then
      p3_filter='programs::install::build::tests::p3_nonce_installed_output_survives_unrelated_update_without_zip_cache_or_hooks'
      p3_extra_filters+=(
        'libraries::package::lease::tests::p3_layout_handoff_reuses_members_without_reacquiring_and_still_verifies_contents'
        'libraries::package::lease::tests::p3_layout_handoff_rejects_same_key_members_from_another_library'
      )
    elif [[ "$p3_manual_mode" == recovery && "$p3_target" == lib ]]; then
      p3_filter='programs::python::p3_recovery_tests::'
      p3_extra_filters+=('programs::shared::lock_transaction::tests::admission::')
    elif [[ "$p3_manual_mode" == source-retained ]]; then
      p3_filter='programs::install::build::tests::'
      p3_extra_filters+=('--skip' 'p3_')
    elif [[ "$p3_manual_mode" == retained ]]; then
      p3_filter='programs::shared::lock_transaction::tests::'
      p3_extra_filters+=(
        'programs::shared::commit::tests::p1_same_project_competing_commits_keep_one_winner_and_other_project'
        'programs::install::lock_document::tests::'
        'programs::query::snapshot::lock_tests::'
        '--skip' 'p3_'
      )
    fi
    cargo test -p pakro "${p3_target_args[@]}" --all-features "$p3_filter" -- ${p3_extra_filters[@]+"${p3_extra_filters[@]}"} --list >"$p3_registered" 2>&1
    cat "$p3_registered"
    p3_count="$(python3 -B -c 'import pathlib,sys; print(sum(line.endswith(": test") for line in pathlib.Path(sys.argv[1]).read_text().splitlines()))' "$p3_registered")"
    if [[ "$p3_manual_mode" == repair || "$p3_manual_mode" == cache ]]; then
      p3_expected_count=3
      if [[ "$p3_target" == bin ]]; then p3_expected_count=5; fi
      if [[ "$p3_count" != "$p3_expected_count" ]]; then
        printf 'P3 %s expected %s %s cases, found %s; refusing an incomplete group\n' "$p3_manual_mode" "$p3_expected_count" "$p3_target" "$p3_count" >&2
        exit 1
      fi
    fi
    if [[ "$p3_count" == 0 ]]; then
      if [[ "$p3_target" == lib ]]; then printf 'No P3 library tests registered\n' >&2; exit 1; fi
      printf 'P3 CLI unit registration has zero cases; skipped, not counted as passed. Real CLI lifecycle remains required.\n'
      continue
    fi
    p3_results="$CARGO_TARGET_DIR/p3-${p3_target}-${p3_manual_stamp}-results.txt"
    cargo test -p pakro "${p3_target_args[@]}" --all-features "$p3_filter" -- ${p3_extra_filters[@]+"${p3_extra_filters[@]}"} --nocapture | tee "$p3_results"
    python3 -B - "$p3_results" "$p3_count" "$p3_manual_mode" <<'PY'
import re, sys
from pathlib import Path
summaries = re.findall(r'^test result: ok\. (\d+) passed; 0 failed; (\d+) ignored;', Path(sys.argv[1]).read_text(), re.MULTILINE)
assert any(int(passed) > 0 and int(passed) + int(ignored) >= int(sys.argv[2]) for passed, ignored in summaries), 'P3 test group did not execute its registered nonzero cases'
if sys.argv[3] in ('repair', 'cache'):
    assert (sys.argv[2], '0') in summaries, 'P3 targeted group must pass every registered case with no ignored parents'
print(f'P3_UNIT_RESULT_VERIFIED registered={sys.argv[2]} summaries={summaries}')
PY
  done
  if [[ "$p3_manual_mode" == repair || "$p3_manual_mode" == cache ]]; then
    cargo build -p pakro --bin pakro --all-features
    if [[ "$p3_manual_mode" == cache ]]; then
      printf 'P3_CACHE_MAIN_BINARY_REFRESHED=true\n'
    else
      printf 'P3_REPAIR_MAIN_BINARY_REFRESHED=true\n'
    fi
  fi
else
  [[ -x "$CARGO_TARGET_DIR/debug/pakro" ]] || { printf 'Run the build group first\n' >&2; exit 1; }
  if [[ "$p3_manual_mode" == extras ]]; then
    bash tests/e2e/run_test tests/e2e/packages/test_p3_extras_closure
  else
    bash tests/e2e/run_test tests/e2e/packages/test_p3_project_lifecycle
  fi
fi
p3_manual_complete=1
