#!/usr/bin/env bash
set -euo pipefail

# End-to-end: bare package names bind to the sole package runtime, and
# package bin entries land in node_modules/.bin and actually run.

export MISE_PYTHON_GITHUB_ATTESTATIONS=0

# python: bare name after declaring the runtime
cat >mise.toml <<'EOF'
[tools]
python = "3.10.14"
EOF
mise i
mise i six@1.17.0
assert ".mise/venvs/python-3.10.14/bin/python -c 'import six; print(six.__version__)'" "1.17.0"

# node: bare name plus .bin execution
cat >mise.toml <<'EOF'
[tools]
node = "22.14.0"
EOF
mise i
mise i typescript@5.8.2
test -f node_modules/.bin/tsc
assert "mise x -- node -e \"console.log(require('typescript').version)\"" "5.8.2"
assert "mise x -- node_modules/.bin/tsc --version" "Version 5.8.2"
