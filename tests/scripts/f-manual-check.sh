#!/usr/bin/env bash
# User-run F-stage verification. Do not invoke this from an agent turn.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
cd "$repo_root"

mode="${1:-f1}"
case "$mode" in
    f1|f2|--list|--format-only) ;;
    *) printf 'Usage: bash tests/scripts/f-manual-check.sh [f1|f2|--list|--format-only]\n' >&2; exit 2 ;;
esac

# Keep one feature selection for every Cargo invocation. Disable the large
# incremental cache and debug symbols for this manual validation only.
export CARGO_TARGET_DIR="$repo_root/target"
export CARGO_INCREMENTAL=0
export CARGO_PROFILE_DEV_DEBUG=0
export CARGO_PROFILE_TEST_DEBUG=0

mkdir -p "$CARGO_TARGET_DIR"
log="$CARGO_TARGET_DIR/f1-manual-$(date '+%Y%m%d-%H%M%S').log"
printf 'Mode: %s. Saving full output to %s\n' "$mode" "$log"
exec > >(tee "$log") 2>&1

f1_format_files=(
    packages/src/python_environment/model.rs
    packages/src/python_environment/model/tests.rs
    packages/src/python_environment/model/codec.rs
    packages/src/python_environment/model/codec/state.rs
    packages/src/python_environment/model/codec/encode.rs
    packages/src/python_environment/model/codec/decode.rs
    packages/src/python_environment/model/codec/tests.rs
    src/core/programs/mod.rs
    src/core/programs/directory.rs
    src/core/programs/install/mod.rs
    src/core/programs/install/apply.rs
    src/core/programs/install/apply/tests.rs
    src/core/libraries/language/mutation.rs
    src/core/libraries/language/mutation/tests.rs
    runtimes/src/plugins/core/python/native/pipeline.rs
    src/core/programs/install/selection.rs
    src/core/programs/install/selection/tests.rs
    src/core/programs/query/mod.rs
    src/core/programs/query/snapshot.rs
    src/core/programs/query/paths.rs
    src/core/programs/query/paths/tests.rs
    src/core/programs/shared/mod.rs
    src/core/programs/shared/commit.rs
    src/core/programs/shared/exact_version.rs
    src/core/programs/shared/exact_version/tests.rs
    src/core/libraries/project/mod.rs
    src/core/libraries/project/identity.rs
    src/core/libraries/project/location.rs
    src/core/libraries/project/repository.rs
    src/core/libraries/project/read_views.rs
    src/core/libraries/language/repository.rs
    src/core/libraries/language/repository/tests.rs
    runtimes/src/plugins/core/python/native.rs
)

check_formatting() {
    local format_status=0
    # Collect every formatting difference in one log, rather than stopping at
    # the first file and requiring another compilation/test run to find more.
    for file in "${f1_format_files[@]}"; do
        rustfmt --edition 2024 --config skip_children=true --check "$file" || format_status=1
    done
    git diff --check || format_status=1
    return "$format_status"
}

if [[ "$mode" == --format-only ]]; then
    printf 'Formatting the F1 file list, then checking it. No compilation or tests.\n'
    for file in "${f1_format_files[@]}"; do
        rustfmt --edition 2024 --config skip_children=true "$file"
    done
    check_formatting
    printf 'F1 format check completed. Log: %s\n' "$log"
    exit 0
fi

if [[ "$mode" == --list ]]; then
    # Confirm every registered full test name exists before counting results.
    cargo test -p pakro --lib --all-features f1_ -- --list
    cargo test -p pakro --lib --all-features f2_ -- --list
    printf 'Registered inventory: tests/scripts/f{1,2}-test-cases.txt\n'
    exit 0
fi

if [[ "$mode" == f2 ]]; then
    # F2 offline unit batch. The e2e downloads a real PBS release (network)
    # and stays in the user lane: tests/e2e/cli/test_f2_exact_python_install.
    cargo test -p pakro --lib --all-features f2_ -- --nocapture
    check_formatting
    printf 'F2 offline batch completed. Review results above. Network e2e (user lane): tests/e2e/cli/test_f2_exact_python_install\n'
    exit 0
fi

# Behavior tests. Before the minimal implementation every f1_ case is expected
# to fail against the registered stubs; that failure is the RED confirmation.
# The env-driven e2e staging helper is excluded from this standalone run.
# Each stage records its own status so one failure does not hide the others.
overall=0
cargo test -p pakro --lib --all-features f1_ -- --nocapture \
    --skip programs::install::selection::tests::f1_e2e_stage_shared_language || overall=1

# Real CLI end-to-end over the original terminal integration. It stages its
# own fixture language offline and builds the pakro binary it exercises.
tests/e2e/run_test tests/e2e/cli/test_f1_directory_selection || overall=1

check_formatting || overall=1

if [[ "$overall" != 0 ]]; then
    printf 'F1 verification FAILED at one or more stages. Review %s\n' "$log"
    exit 1
fi
printf 'F1 verification commands completed. Review actual test counts and failures in %s\n' "$log"
