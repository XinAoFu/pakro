#!/usr/bin/env bash
set -euo pipefail

# End-to-end: python runtime + native numpy package, no pip/uv anywhere in the
# process tree. Verifies store sharing across two projects and lock reuse.

export MISE_PYTHON_GITHUB_ATTESTATIONS=0

cat > mise.toml <<'EOF'
[tools]
python = "3.10.14"

[packages."python@3.10.14"]
numpy = "1.26.4"
EOF

mise i

# the generated venv imports numpy without pip ever running
VENV_PY=".mise/venvs/python-3.10.14/bin/python"
test -f "$VENV_PY"
assert "$VENV_PY -c 'import numpy; print(numpy.__version__)'" "1.26.4"

# the graph lockfile records the exact pin and digest
test -f mise.packages.lock
assert_contains "cat mise.packages.lock" 'version = "1.26.4"'
assert_contains "cat mise.packages.lock" 'sha256-'

# a second project with the same pins downloads nothing new
mkdir -p proj2
cp mise.toml proj2/mise.toml
(
  cd proj2
  mise i
  assert "./.mise/venvs/python-3.10.14/bin/python -c 'import numpy; print(numpy.__version__)'" "1.26.4"
)

# the store holds exactly one numpy entry shared by both projects
NUMPY_ENTRIES=$(find "${MISE_DATA_DIR}/store/python/numpy" -name .mise-complete | wc -l | tr -d ' ')
assert "echo $NUMPY_ENTRIES" "1"
