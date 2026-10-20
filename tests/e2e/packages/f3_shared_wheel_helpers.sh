#!/usr/bin/env bash
# Sourced by the existing run_test harness inside its isolated HOME.
f3_setup() {
  export CARGO_TARGET_DIR="$ROOT/target"
  export CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
  export NO_PROXY=127.0.0.1,localhost,::1
  export no_proxy="$NO_PROXY"
  export PAKRO_PYTHON_GITHUB_ATTESTATIONS=0 PAKRO_PYTHON_COMPILE=0
  local stage_log="$PAKRO_CACHE_DIR/f3-language-stage.log"
  (
    cd "$ROOT"
    PAKRO_F3_STAGE_DATA_ROOT="$PAKRO_DATA_DIR" cargo test -p pakro --lib --all-features \
      programs::python::integration_tests::f3_e2e_stage_real_language -- --exact --ignored --nocapture
  ) >"$stage_log" 2>&1 || { cat "$stage_log"; fail "real PBS language staging failed"; }
  F3_STAGED_PYTHON="$(sed -n 's/.*F3_STAGED_PYTHON=//p' "$stage_log" | tail -1)"
  [[ -x $F3_STAGED_PYTHON ]] || fail "real staging helper did not return a published interpreter"
  [[ $("$F3_STAGED_PYTHON" --version) == 'Python 3.13.16' ]] || fail "unexpected real interpreter"
  F3_FIXTURE_DIR="$TMPDIR/f3-wheels"
  F3_REQUEST_LOG="$TMPDIR/f3-http-requests"
  mkdir -p "$F3_REQUEST_LOG"
  python3 "$TEST_DIR/f3_shared_wheel_fixture.py" "$F3_FIXTURE_DIR"
  start_http_server --headers-log "$F3_REQUEST_LOG" --handler "$TEST_DIR/f3_shared_wheel_fixture.py" "$F3_FIXTURE_DIR"
  export PAKRO_URL_REPLACEMENTS="{\"https://pypi.org\":\"http://127.0.0.1:$HTTP_PORT\"}"
  export PAKRO_F3_MANAGER_LOG="$TMPDIR/f3-manager-invocations"
  for manager in pip pip3 uv; do
    cat >"$HOME/bin/$manager" <<'SH'
#!/bin/sh
printf '%s\n' "$0" >> "$PAKRO_F3_MANAGER_LOG"
exit 91
SH
    chmod +x "$HOME/bin/$manager"
  done
}
f3_declare() {
  mkdir -p "$1"
  printf '[tools]\npython = "3.13.16"\n[packages."python@3.13.16"]\n%s = "1.0"\n' "$2" >"$1/pakro.toml"
}
f3_request_count() { find "$F3_REQUEST_LOG" -type f -name 'request_*.json' | wc -l | tr -d ' '; }
f3_package_count() { find "$PAKRO_DATA_DIR/packages/.control/catalog" -type f -name 'package-*.json' | wc -l | tr -d ' '; }
f3_assert_no_manager() { [[ ! -e $PAKRO_F3_MANAGER_LOG ]] || fail "external package manager was called: $(cat "$PAKRO_F3_MANAGER_LOG")"; }
