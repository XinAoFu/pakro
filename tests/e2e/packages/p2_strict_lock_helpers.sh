#!/usr/bin/env bash
# Sourced only by the existing isolated run_test environment.
source "$TEST_DIR/f3_shared_wheel_helpers.sh"
[[ $(uname -s) == Darwin && $(uname -m) == arm64 ]] || fail 'this verified PBS fixture requires macOS arm64'
: "${PAKRO_TEST_PYTHON_ARCHIVE:?set the verified original Python 3.13.16 stripped archive}"
python3 "$TEST_ROOT/cli/p1_exact_python_fixture.py" "$PAKRO_TEST_PYTHON_ARCHIVE"
export NO_PROXY=127.0.0.1,localhost,::1
export no_proxy="$NO_PROXY"
unset GITHUB_TOKEN PAKRO_GITHUB_TOKEN
export PAKRO_OFFLINE=0 PAKRO_AUTO_UPDATE=0 PAKRO_CACHE_PRUNE_AGE=0 PAKRO_REGISTRY_FLOATING=0
export PAKRO_PYTHON_GITHUB_ATTESTATIONS=0 PAKRO_PYTHON_COMPILE=0
F3_FIXTURE_DIR="$TMPDIR/p2-lock-fixture"
F3_REQUEST_LOG="$TMPDIR/p2-lock-requests"
mkdir -p "$F3_REQUEST_LOG"
python3 "$TEST_DIR/p2_lock_fixture.py" "$F3_FIXTURE_DIR"
start_http_server --headers-log "$F3_REQUEST_LOG" --handler "$TEST_DIR/p2_lock_fixture.py" "$PAKRO_TEST_PYTHON_ARCHIVE" "$F3_FIXTURE_DIR"
export PAKRO_URL_REPLACEMENTS="{\"https://pypi.org\":\"http://127.0.0.1:$HTTP_PORT\",\"https://github.com\":\"http://127.0.0.1:$HTTP_PORT\",\"https://api.github.com\":\"http://127.0.0.1:$HTTP_PORT\"}"
export PAKRO_F3_MANAGER_LOG="$TMPDIR/p2-manager-invocations"
for p2_manager in pip pip3 uv; do
  cat >"$HOME/bin/$p2_manager" <<'SH'
#!/bin/sh
printf '%s\n' "$0" >> "$PAKRO_F3_MANAGER_LOG"
exit 91
SH
  chmod +x "$HOME/bin/$p2_manager"
done

p2_workspace="$PWD"
mkdir generated cold rejected
cat >generated/pakro.toml <<TOML
[tools]
python = "3.13.16+20261013"
[settings.python]
precompiled_flavor = "install_only_stripped"
github_attestations = false
compile = false
[packages."python@3.13.16+20261013"]
$P2_ROOT_PACKAGE = "1.0"
TOML
cat >generated/pakro.lock <<'TOML'
# Preserve the native header and unknown future section exactly.
lockfile_version = 3 # preserve this inline comment
[[tools.node]]
version = '99.0.0' # unrelated native tool entry
[future]
opaque = 'unchanged-future-value'
TOML
p2_declarations_before="$(cksum <generated/pakro.toml)"
for p2_library in languages packages projects; do
  [[ ! -e "$PAKRO_DATA_DIR/$p2_library" ]] || fail "cold generator already has $p2_library"
done
(cd generated && pakro install)
[[ $(cksum <generated/pakro.toml) == "$p2_declarations_before" ]] || fail 'lock generation edited declarations'
grep -Eq '^\[python\]$' generated/pakro.lock || fail 'no unified Python lock was generated'
grep -Eq '^declaration_fingerprint[[:space:]]*=' generated/pakro.lock || fail 'lock lacks a declaration commitment'
grep -Fq "version = '99.0.0' # unrelated native tool entry" generated/pakro.lock || fail 'native tool bytes were rewritten'
grep -Fq "opaque = 'unchanged-future-value'" generated/pakro.lock || fail 'unknown lock section was lost'
grep -Fq '# Preserve the native header and unknown future section exactly.' generated/pakro.lock || fail 'native comments were lost'
if [[ $P2_ROOT_PACKAGE == p2-source ]]; then
  grep -Fq '[[python.builds]]' generated/pakro.lock || fail 'source lock lacks original build inputs and output'
  grep -Fq '[python.builds.observation]' generated/pakro.lock || fail 'source lock lacks initial/dynamic metadata observations'
fi
p2_lock_before="$(cksum <generated/pakro.lock)"
p2_lock_inode="$(python3 -c 'import os; print(os.stat("generated/pakro.lock").st_ino)')"
p2_requests_before="$(f3_request_count)"
(cd generated && PAKRO_LOCKED=1 pakro deps install python)
[[ $(f3_request_count) == "$p2_requests_before" ]] || fail 'warm locked replay made HTTP requests'
[[ $(cksum <generated/pakro.lock) == "$p2_lock_before" ]] || fail 'warm strict replay changed lock bytes'
[[ $(python3 -c 'import os; print(os.stat("generated/pakro.lock").st_ino)') == "$p2_lock_inode" ]] || fail 'warm locked replay replaced the lock inode'

# Offer a newer version satisfying z-root/source's transitive range. A fresh
# solver would select 1.1; strict replay must keep the original exact 1.0.
touch "$F3_FIXTURE_DIR/new-candidates"
cp generated/pakro.toml generated/pakro.lock cold/
export PAKRO_DATA_DIR="$TMPDIR/p2-cold-data" PAKRO_CACHE_DIR="$TMPDIR/p2-cold-cache"
mkdir -p "$PAKRO_DATA_DIR" "$PAKRO_CACHE_DIR"
(cd cold && PAKRO_LOCKED=1 pakro install)
[[ $(cksum <cold/pakro.lock) == "$p2_lock_before" ]] || fail 'cold strict replay changed lock bytes'
[[ $(cksum <cold/pakro.toml) == "$p2_declarations_before" ]] || fail 'cold strict replay changed declarations'
python3 - "$F3_REQUEST_LOG" <<'PY'
import json, sys
from pathlib import Path
paths = [json.loads(path.read_text())['path'] for path in Path(sys.argv[1]).glob('request_*.json')]
assert not any('a_dep-1.1' in path for path in paths), paths
print('P2 locked cold replay did not acquire the newly available transitive version')
PY
cd cold
eval "$(pakro activate bash)"
eval "$(pakro hook-env -s bash --force)"
if [[ $P2_ROOT_PACKAGE == p2-source ]]; then
  assert "python -c 'import p2_source,a_dep; print(a_dep.VALUE)'" 'shared-dependency'
  assert 'p2-source-cli' 'source:shared-dependency'
else
  assert "python -c 'import z_root,a_dep,pth_marker; print(a_dep.VALUE + \":\" + pth_marker.VALUE)'" 'shared-dependency:pth-visible'
  assert 'z-root-cli' 'shared-dependency:pth-visible'
fi
p2_cold_requests="$(f3_request_count)"
PAKRO_LOCKED=1 pakro install
[[ $(f3_request_count) == "$p2_cold_requests" ]] || fail 'second restored install fetched complete existing content'
[[ $(cksum <pakro.lock) == "$p2_lock_before" ]] || fail 'second restored install changed the exact lock'
p2_language_scope_before="$(pakro where python)"
cp pakro.toml "$TMPDIR/p2-original-package-declarations"
python3 - pakro.toml "$P2_ROOT_PACKAGE" <<'PY'
import sys
from pathlib import Path
path, package = Path(sys.argv[1]), sys.argv[2]
body = path.read_text()
old = package + ' = "1.0"'
assert old in body
path.write_text(body.replace(old, package + ' = "1.1"', 1))
PY
assert 'pakro where python' "$p2_language_scope_before"
assert_fail_contains 'pakro env --json 2>&1' 'declaration'
[[ $(f3_request_count) == "$p2_cold_requests" ]] || fail 'language/package scope queries acquired content'
[[ $(cksum <pakro.lock) == "$p2_lock_before" ]] || fail 'language/package scope queries modified the exact lock'
cp "$TMPDIR/p2-original-package-declarations" pakro.toml
cd "$p2_workspace"
cp generated/pakro.toml generated/pakro.lock rejected/
printf '\n# external declaration edit\n' >>rejected/pakro.toml
p2_rejected_requests="$(f3_request_count)"
p2_rejected_catalog="$(cksum <"$PAKRO_DATA_DIR/projects/.control/catalog/projects")"
assert_fail_contains '(cd rejected && PAKRO_LOCKED=1 pakro install) 2>&1' 'declaration'
[[ $(f3_request_count) == "$p2_rejected_requests" ]] || fail 'changed declaration accessed a source before strict rejection'
[[ $(cksum <"$PAKRO_DATA_DIR/projects/.control/catalog/projects") == "$p2_rejected_catalog" ]] || fail 'rejected strict request changed project selection'
[[ $(cksum <rejected/pakro.lock) == "$p2_lock_before" ]] || fail 'rejected strict request silently changed the lock'
if [[ $P2_ROOT_PACKAGE == p2-source ]]; then
  cp cold/pakro.lock "$TMPDIR/p2-valid-source-lock"
  python3 - cold/pakro.lock <<'PY'
import re, sys
from pathlib import Path
path = Path(sys.argv[1])
body = path.read_text()
head, separator, builds = body.partition('[[python.builds]]')
assert separator, 'source fixture has no build evidence'
builds, count = re.subn(r'(?m)^output_digest = "sha256-[0-9a-f]{64}"$',
                        'output_digest = "sha256-' + '0' * 64 + '"', builds, count=1)
assert count == 1, 'source fixture has no exact output digest'
path.write_text(head + separator + builds)
PY
  p2_changed_output="$(cksum <cold/pakro.lock)"
  p2_before_output_rejection="$(cksum <"$PAKRO_DATA_DIR/projects/.control/catalog/projects")"
  assert_fail_matches '(cd cold && PAKRO_LOCKED=1 pakro install) 2>&1' 'output|build|locked'
  [[ $(cksum <cold/pakro.lock) == "$p2_changed_output" ]] || fail 'different rebuilt output silently rewrote the lock'
  [[ $(cksum <"$PAKRO_DATA_DIR/projects/.control/catalog/projects") == "$p2_before_output_rejection" ]] || fail 'different rebuilt output changed the selected project graph'
  cp "$TMPDIR/p2-valid-source-lock" cold/pakro.lock
fi
f3_assert_no_manager
printf 'P2_STRICT_REPLAY_PASSED package=%s cold_runtime=true cold_packages=true warm_http=0\n' "$P2_ROOT_PACKAGE"
