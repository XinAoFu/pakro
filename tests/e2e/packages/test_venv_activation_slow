#!/usr/bin/env bash
set -euo pipefail

# End-to-end: the materialized package venv is part of the mise environment
# (ADR-0007). `mise x -- python` resolves into the venv and sees its
# packages, VIRTUAL_ENV points at the venv root, and bin/activate is a
# sourceable POSIX script for shells and IDEs outside mise.

export MISE_PYTHON_GITHUB_ATTESTATIONS=0

cat >mise.toml <<'EOF'
[tools]
python = "3.10.14"

[packages."python@3.10.14"]
six = "1.17.0"
EOF

mise i

# mise x runs the venv python, not the bare runtime
assert_contains "mise x -- python -c 'import sys; print(sys.executable)'" "venvs/python-3.10.14/bin/python"
assert "mise x -- python -c 'import six; print(six.__version__)'" "1.17.0"

# the environment exposes VIRTUAL_ENV at the venv root
assert_contains "mise x -- python -c 'import os; print(os.environ[\"VIRTUAL_ENV\"])'" ".mise/venvs/python-3.10.14"

# the activate script is a working standard venv activation
test -f .mise/venvs/python-3.10.14/bin/activate
assert_contains "sh -c '. .mise/venvs/python-3.10.14/bin/activate && python -c \"import six, sys; print(sys.prefix)\"'" ".mise/venvs/python-3.10.14"
