#!/usr/bin/env bash
# Focused F1-F4 checks; every cargo call shares one small build configuration.
set -euo pipefail
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
cd "$repo_root"
mode="${1:-f1}"
case "$mode" in f1|f2|f3|f4|--list|--format-only) ;; *) printf 'Usage: %s [f1|f2|f3|f4|--list|--format-only] [--lib-only]\n' "$0" >&2; exit 2 ;; esac
export CARGO_TARGET_DIR="$repo_root/target"
export CARGO_INCREMENTAL=0
export CARGO_PROFILE_DEV_DEBUG=0
export CARGO_PROFILE_TEST_DEBUG=0
mkdir -p "$CARGO_TARGET_DIR"
log="$CARGO_TARGET_DIR/${mode#--}-manual-$(date '+%Y%m%d-%H%M%S').log"
printf 'Mode: %s. Full output: %s\n' "$mode" "$log"
exec > >(tee "$log") 2>&1

format_check() {
    local status=0
    while IFS= read -r path; do
        [[ -f "$path" && "$path" == *.rs ]] || continue
        rustfmt --edition 2024 --config skip_children=true --check "$path" || status=1
    done < <({ git diff --name-only; git ls-files --others --exclude-standard; } | sort -u)
    git diff --check || status=1
    return "$status"
}
if [[ "$mode" == --format-only ]]; then format_check; exit; fi

list_stage() {
    local stage="$1" output="$CARGO_TARGET_DIR/${1}-registered-tests.txt"
    local package_args=(-p pakro)
    [[ "$stage" != f3 ]] || package_args+=(-p pakro-index)
    cargo test "${package_args[@]}" --lib --all-features "${stage}_" -- --list \
        --skip f1_e2e_stage_shared_language --skip f3_e2e_stage_real_language --skip f4_e2e_stage_library >"$output" 2>&1
    cat "$output"
    # No empty filters may masquerade as a successful stage.
    if ! rg -q "${stage}_.*: test$" "$output"; then
        printf 'No registered %s tests found.\n' "$stage" >&2; return 1
    fi
    local line name
    while IFS= read -r line; do
        [[ -n "$line" && "$line" != \#* && "$line" != *"[ignored:"* && "$line" != *"[network:"* ]] || continue
        if [[ "$line" == 'pakro lib '* ]]; then name="${line#pakro lib }";
        elif [[ "$line" == 'pakro-index lib '* ]]; then name="${line#pakro-index lib }";
        elif [[ "$line" == *'::'* ]]; then name="$line";
        else continue; fi
        if ! rg -Fqx "$name: test" "$output"; then
            printf 'Missing registered test: %s\n' "$name" >&2; return 1
        fi
    done <"tests/scripts/${stage}-test-cases.txt"
    if [[ "$stage" == f4 ]]; then
        local store_output="$CARGO_TARGET_DIR/f4-store-registered-tests.txt"
        cargo test -p pakro-store --lib control::tests::f4_store_initialization_qualification_is_not_reissued -- --list >"$store_output" 2>&1
        cat "$store_output"
        rg -Fqx 'control::tests::f4_store_initialization_qualification_is_not_reissued: test' "$store_output"
    fi
    if [[ "$stage" == f1 ]]; then
        local cli_output="$CARGO_TARGET_DIR/f1-cli-registered-tests.txt"
        cargo test -p pakro --bin pakro --all-features f1_bytecode_recovery_classifier_ -- --list >"$cli_output" 2>&1
        cat "$cli_output"
        rg -Fqx 'cli::tests::f1_bytecode_recovery_classifier_selects_explicit_install_even_if_incomplete: test' "$cli_output"
        rg -Fqx 'cli::tests::f1_bytecode_recovery_classifier_preserves_other_commands_values_and_double_dash: test' "$cli_output"
    fi
}
if [[ "$mode" == --list ]]; then
    for stage in f1 f2 f3 f4; do list_stage "$stage"; done
    exit
fi
list_stage "$mode"
package_args=(-p pakro)
[[ "$mode" != f3 ]] || package_args+=(-p pakro-index)
cargo test "${package_args[@]}" --lib --all-features "${mode}_" -- --nocapture \
    --skip f1_e2e_stage_shared_language --skip f3_e2e_stage_real_language --skip f4_e2e_stage_library
if [[ "$mode" == f4 ]]; then
    cargo test -p pakro-store --lib control::tests::f4_store_initialization_qualification_is_not_reissued -- --exact --nocapture
fi
if [[ "$mode" == f1 ]]; then
    cargo test -p pakro --bin pakro --all-features f1_bytecode_recovery_classifier_ -- --nocapture
    cargo test -p pakro-store --lib f1_recovery_prepared_cas -- --nocapture
fi
if [[ "${2:-}" != --lib-only ]]; then
    cargo build -p pakro --all-features
    case "$mode" in
      f1)
        tests/e2e/run_test tests/e2e/cli/test_f1_directory_selection
        tests/e2e/run_test tests/e2e/cli/test_f1_shared_python_bytecode
        tests/e2e/run_test tests/e2e/cli/test_python_bytecode_recovery
        tests/e2e/run_test tests/e2e/cli/test_f1_command_lookup_no_install ;;
      f2)
        tests/e2e/run_test tests/e2e/cli/test_f2_install_progress
        tests/e2e/run_test tests/e2e/cli/test_f2_exact_python_install ;;
      f3)
        tests/e2e/run_test tests/e2e/packages/test_native_install_shared_wheel
        tests/e2e/run_test tests/e2e/packages/test_native_deps_install_preserves_declarations
        tests/e2e/run_test tests/e2e/packages/test_native_deps_proxy
        tests/e2e/run_test tests/e2e/packages/test_native_deps_no_manager ;;
      f4)
        tests/e2e/run_test tests/e2e/cli/test_f4_library_query_and_uninstall
        tests/e2e/run_test tests/e2e/cli/test_f4_python_queries_are_readonly ;;
    esac
fi
format_check
printf '%s checks completed; inspect the actual counts and e2e results in %s\n' "$mode" "$log"
