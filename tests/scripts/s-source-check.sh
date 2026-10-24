#!/usr/bin/env bash
# Run under the caller's single 600-second watchdog; full ISAT is a separate group.
set -euo pipefail
source_repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
cd "$source_repo_root"
export CARGO_TARGET_DIR="$source_repo_root/target"
export CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
source_mode="${1:---lib-only}"
case "$source_mode" in
  --lib-only|--core-isat|--full-isat|--full-isat-fixture) ;;
  *) printf 'Usage: %s [--lib-only|--core-isat|--full-isat|--full-isat-fixture ABS_PATH]\n' "$0" >&2; exit 2 ;;
esac
if [[ "$source_mode" == --lib-only ]]; then
  /usr/bin/python3 -B tests/e2e/packages/isat_release_fixture.py --self-test
  source_fd_list="$CARGO_TARGET_DIR/source-fd-capacity-registered-tests.txt"
  cargo test -p pakro-util --lib 'process_limits::tests::' -- --list >"$source_fd_list" 2>&1
  cat "$source_fd_list"
  rg -q '^process_limits::tests::s4_fd_capacity_.*: test$' "$source_fd_list"
  cargo test -p pakro-util --lib 'process_limits::tests::' -- --nocapture
  for source_filter in s1_source_build_ s2_ s3_ s4_ f3_cow_clone_ f3_native_ f3_layout_legacy_recipe_; do
    source_list="$CARGO_TARGET_DIR/source-${source_filter}-registered-tests.txt"
    cargo test -p pakro -p pakro-index --lib --all-features "$source_filter" -- --list >"$source_list" 2>&1
    cat "$source_list"
    rg -q "${source_filter}.*: test$" "$source_list"
    cargo test -p pakro -p pakro-index --lib --all-features "$source_filter" -- --nocapture
  done
  source_policy_list="$CARGO_TARGET_DIR/source-policy-registered-tests.txt"
  cargo test -p pakro-resolver --lib 'python_native::policy::tests::' -- --list >"$source_policy_list" 2>&1
  cat "$source_policy_list"
  rg -q '^python_native::policy::tests::policy_rejection_.*: test$' "$source_policy_list"
  cargo test -p pakro-resolver --lib 'python_native::policy::tests::' -- --nocapture
  source_rank_list="$CARGO_TARGET_DIR/source-wheel-rank-registered-tests.txt"
  cargo test -p pakro-index --test wheel_rank --all-features -- --list >"$source_rank_list" 2>&1
  cat "$source_rank_list"
  rg -q '^s4_rank_plan_.*: test$' "$source_rank_list"
  cargo test -p pakro-index --test wheel_rank --all-features -- --nocapture
else
  # Build first in a separate bounded group so the cold acceptance gets its own deadline.
  [[ -x "$CARGO_TARGET_DIR/debug/pakro" && -x "$CARGO_TARGET_DIR/debug/deps/pakro-80f217ff598af8de" ]]
  if [[ "$source_mode" == --full-isat-fixture ]]; then
    ROOT="$source_repo_root" /usr/bin/python3 -B tests/e2e/packages/source_build_acceptance.py \
      --full --release-fixture "${2:?release fixture requires an absolute corpus path}"
  elif [[ "$source_mode" == --core-isat ]]; then
    tests/e2e/run_test tests/e2e/packages/test_native_source_isat
  else
    tests/e2e/run_test tests/e2e/packages/test_native_source_isat_full
  fi
fi
