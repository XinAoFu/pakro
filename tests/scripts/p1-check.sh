#!/usr/bin/env bash
# Each invocation is one independently bounded group, with one Cargo at a time.
set -euo pipefail
p1_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
cd "$p1_root"
export CARGO_TARGET_DIR="$p1_root/target"
export CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
p1_mode="${1:---list}"
case "$p1_mode" in
  --build|--lib|--cli-unit|--e2e-empty|--e2e-shared|--review|--watchdog-test|--exact-install|--f-regression|--source|--resources|--list|--format) ;;
  *) printf 'Unknown P1 group: %s\n' "$p1_mode" >&2; exit 2;;
esac
if [[ ${PAKRO_P1_WATCHDOG:-0} != 1 ]]; then
  exec python3 - "$p1_root" "$p1_mode" <<'PY'
import importlib.util, os, pathlib, shutil, sys, tempfile, time
root=pathlib.Path(sys.argv[1]); mode=sys.argv[2]
(root/'target').mkdir(exist_ok=True)
log=root/'target'/f'p1-{mode.removeprefix("--")}-{time.strftime("%Y%m%d-%H%M%S")}.log'
temporary=os.path.realpath(tempfile.mkdtemp(prefix='pakro-p1-',dir='/tmp'))
env=dict(os.environ,PAKRO_P1_WATCHDOG='1',TMPDIR=temporary);env.setdefault('RUST_BACKTRACE','0')
def cleanup_missing_only(action, path, error):
    if not isinstance(error[1], FileNotFoundError):raise error[1]
spec=importlib.util.spec_from_file_location('p1_watchdog',root/'tests/scripts/p1-watchdog.py')
watchdog=importlib.util.module_from_spec(spec);spec.loader.exec_module(watchdog)
print(f'P1 group {mode}; limit 600 seconds; log {log}',flush=True)
with log.open('w') as output:
    output.write(f'group={mode} limit_seconds=600\n');output.flush()
    result=watchdog.supervise(['bash',str(root/'tests/scripts/p1-check.sh'),mode],output,timeout=600,env=env)
    code=result['exit']
    output.write(f'\nP1_SUPERVISION {result}\n')
    output.write(f"P1_GROUP elapsed_seconds={result['elapsed_seconds']:.3f} exit={code}\n")
print(''.join(log.read_text().splitlines(keepends=True)[-70:]),flush=True)
print(f'Complete output: {log}',flush=True)
if code==0 or not result.get('launched', True):
    shutil.rmtree(temporary,onerror=cleanup_missing_only)
    if not result.get('launched', True):
        print(result.get('reason','strict supervision unavailable'),flush=True)
        print('User terminal entry: bash tests/scripts/p1-manual-recheck.sh all',flush=True)
else:print(f'P1 owned failed-test root: {temporary}',flush=True)
sys.exit(code if code>=0 else 128-code)
PY
fi
p1_verify_manifest() {
  local output="$1" kind="$2" package target name
  rg -q ': test$' "$output"
  while read -r package target name; do
    [[ "$package" == pakro && "$target" == "$kind" ]] || continue
    rg -Fqx "$name: test" "$output" || { printf 'Missing registration: %s\n' "$name" >&2; return 1; }
  done <tests/scripts/p1-test-cases.txt
}
p1_filter() {
  local filter="$1" kind="${2:---lib}"
  cargo test -p pakro "$kind" --all-features "$filter" -- --list >"$CARGO_TARGET_DIR/p1-registered-${filter//:/_}.txt" 2>&1
  cat "$CARGO_TARGET_DIR/p1-registered-${filter//:/_}.txt"
  rg -q ': test$' "$CARGO_TARGET_DIR/p1-registered-${filter//:/_}.txt"
  if [[ "$filter" == p1_ ]]; then p1_verify_manifest "$CARGO_TARGET_DIR/p1-registered-${filter//:/_}.txt" lib; fi
  if [[ "$filter" == programs::directory::tests:: ]]; then
    cargo test -p pakro "$kind" --all-features "$filter" -- --nocapture --skip p1_
  else
    cargo test -p pakro "$kind" --all-features "$filter" -- --nocapture
  fi
}
case "$p1_mode" in
  --build) cargo build -p pakro --bin pakro --all-features;;
  --lib) p1_filter p1_; p1_filter programs::directory::tests::; p1_filter programs::install::apply::tests::;;
  --cli-unit)
    cargo test -p pakro --bin pakro --all-features cli:: -- --list >"$CARGO_TARGET_DIR/p1-cli-registered.txt" 2>&1
    cat "$CARGO_TARGET_DIR/p1-cli-registered.txt"
    p1_verify_manifest "$CARGO_TARGET_DIR/p1-cli-registered.txt" bin
    while read -r package target name; do
      [[ "$package" == pakro && "$target" == bin ]] || continue
      cargo test -p pakro --bin pakro --all-features "$name" -- --exact --nocapture
    done <tests/scripts/p1-test-cases.txt;;
  --e2e-empty)
    for case in test_p1_shared_python_empty_queries test_p1_python_write_admission test_p1_readonly_initialization test_p1_non_python_command_dispatch test_p1_query_option_admission; do
      tests/e2e/run_test "tests/e2e/cli/$case"
    done;;
  --e2e-shared)
    : "${PAKRO_TEST_PYTHON_ARCHIVE:?set the verified local Python 3.13.16 archive}"
    tests/e2e/run_test tests/e2e/cli/test_p1_shared_python_queries;;
  --review)
    for case in test_p1_configured_python_package_admission test_p1_appended_env_and_companion_queries test_p1_mixed_env_is_readonly; do
      tests/e2e/run_test "tests/e2e/cli/$case"
    done;;
  --watchdog-test) python3 tests/scripts/test-p1-watchdog.py;;
  --exact-install)
    : "${PAKRO_TEST_PYTHON_ARCHIVE:?set the verified local Python 3.13.16 archive}"
    tests/e2e/run_test tests/e2e/cli/test_p1_exact_python_install;;
  --f-regression)
    for filter in f1_ f2_ f3_ f4_; do p1_filter "$filter"; done;;
  --source) bash tests/scripts/s-source-check.sh --lib-only;;
  --resources)
    cargo test -p pakro --lib --all-features --no-run --message-format=json >"$CARGO_TARGET_DIR/p1-resource-build.jsonl"
    p1_binary="$(python3 - "$CARGO_TARGET_DIR/p1-resource-build.jsonl" <<'PY'
import json,sys
found=[]
for line in open(sys.argv[1]):
    try:entry=json.loads(line)
    except ValueError:continue
    if entry.get('reason')=='compiler-artifact' and entry.get('profile',{}).get('test') and entry.get('executable'):
        found.append(entry['executable'])
if len(found)!=1:raise SystemExit('Expected one actual test executable')
print(found[0])
PY
)"
    python3 tests/scripts/p1-resource-baseline.py --test-binary "$p1_binary" --output "$CARGO_TARGET_DIR/p1-resource-baseline.json"
    p1_filter libraries::package::layouts::tests::file_descriptors::s4_file_descriptor_capacity_allows_sixty_four_real_package_trees
    cargo test -p pakro-util --lib process_limits::tests:: -- --nocapture;;
  --list)
    cargo test -p pakro --lib --all-features p1_ -- --list >"$CARGO_TARGET_DIR/p1-lib-registered.txt" 2>&1
    cat "$CARGO_TARGET_DIR/p1-lib-registered.txt"
    p1_verify_manifest "$CARGO_TARGET_DIR/p1-lib-registered.txt" lib
    while read -r kind path; do [[ "$kind" != e2e || -f "$path" ]]; done <tests/scripts/p1-test-cases.txt
    cat tests/scripts/p1-test-cases.txt;;
  --format)
    while IFS= read -r path; do
      [[ -f "$path" && "$path" == *.rs ]] || continue
      rustfmt --edition 2024 --config skip_children=true --check "$path"
    done < <({ git diff --name-only; git ls-files --others --exclude-standard; } | sort -u)
    git diff --check
    bash -n tests/scripts/p1-check.sh
    python3 -c 'import ast,pathlib; ast.parse(pathlib.Path("tests/scripts/p1-resource-baseline.py").read_text())';;
esac
