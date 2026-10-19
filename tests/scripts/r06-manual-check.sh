#!/usr/bin/env bash
# User-run R06 verification. Do not invoke this from an agent turn.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
cd "$repo_root"

mode="${1:-all}"
case "$mode" in
    all|--remaining|--format-only) ;;
    *) printf 'Usage: bash tests/scripts/r06-manual-check.sh [--remaining|--format-only]\n' >&2; exit 2 ;;
esac

# Keep one feature selection for every Cargo invocation. Disable the large
# incremental cache and debug symbols for this manual validation only.
export CARGO_TARGET_DIR="$repo_root/target"
export CARGO_INCREMENTAL=0
export CARGO_PROFILE_DEV_DEBUG=0
export CARGO_PROFILE_TEST_DEBUG=0
export PAKRO_R06_ARCHIVE='/Users/fuxinao/文件/project/pakro_test/cpython-3.13.16+20261013-aarch64-apple-darwin-install_only_stripped.tar.gz'
export PAKRO_R06_REQUEST='3.13.16'
export PAKRO_R06_SHA256='16a5332fb0dcb77f163fc8f8cbccd95be1644cc53e68e5d2a79f50afd6680a0e'

if [[ "$mode" == all && ! -f "$PAKRO_R06_ARCHIVE" ]]; then
    printf 'Missing explicitly supplied archive: %s\n' "$PAKRO_R06_ARCHIVE" >&2
    exit 1
fi

mkdir -p "$CARGO_TARGET_DIR"
log="$CARGO_TARGET_DIR/r06-manual-$(date '+%Y%m%d-%H%M%S').log"
printf 'Mode: %s. Saving full output to %s\n' "$mode" "$log"
exec > >(tee "$log") 2>&1
printf 'R06 verification mode: %s\n' "$mode"

check_formatting() {
    local format_status=0
    # Collect every formatting difference in one log, rather than stopping at
    # the first file and requiring another compilation/test run to discover it.
    while IFS= read -r file; do
        rustfmt --edition 2024 --config skip_children=true --check "$file" || format_status=1
    done < tests/scripts/r06-format-files.txt
    git diff --check || format_status=1
    return "$format_status"
}

if [[ "$mode" == --format-only ]]; then
    printf 'Formatting the R06 file list, then checking it. No compilation or tests.\n'
    while IFS= read -r file; do
        rustfmt --edition 2024 --config skip_children=true "$file"
    done < tests/scripts/r06-format-files.txt
    check_formatting
    printf 'Selected R06 verification commands completed (mode: %s). Log: %s\n' "$mode" "$log"
    exit 0
fi

packages=(-p pakro -p pakro-util -p pakro-sigstore)

# Include the two real-distribution parents. Only their subprocess helpers are
# excluded from the outer invocation; each parent runs its own actual children.
if [[ "$mode" == all ]]; then
    cargo test "${packages[@]}" --lib --all-features 'r06_' -- \
        --include-ignored --nocapture \
        --skip libraries::language::repository::tests::r06_process_helper \
        --skip libraries::language::repository::tests::r06_real_process_helper
else
    # Resume after r06-manual-20261018-180052.log: the unchanged main library's
    # 84 R06 cases passed. Retry the failing Sigstore fixtures and run util,
    # which did not execute after Cargo stopped at the Sigstore failure.
    for filter in \
        'tests::frozen_digest::r06_' \
        'tests::frozen_route::r06_' \
        'github::sigstore::tests::r06_'
    do
        cargo test "${packages[@]}" --lib --all-features "$filter" -- --nocapture
    done
fi

# Adjacent callers of the changed cores. Keeping all selected packages avoids
# rebuilding dependencies under a second feature combination. A crate without
# a matching filter reports zero here; that is not a passed test for that crate.
for filter in \
    'plugins::core::python::native::pipeline::tests::' \
    'plugins::core::python::native::extract::tests::' \
    'plugins::core::python::tests::native_' \
    'packages::python_environment::prefix::verify::tests::' \
    'sysconfig::tests::'
do
    cargo test "${packages[@]}" --lib --all-features "$filter" -- --nocapture
done

cargo check "${packages[@]}" --lib --all-features

# Check only the R06 files, preserving unrelated worktree formatting.
check_formatting

printf 'Selected R06 verification commands completed (mode: %s). Review actual test counts and ignored cases in %s\n' "$mode" "$log"
