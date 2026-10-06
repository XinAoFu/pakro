#!/usr/bin/env bash
set -euo pipefail

# Project command family: init scaffolds config with runtime+package pins,
# attach brings existing code under management, ls/status report state.

mise project init proj --python 3.10.14 --package python+six@1.17.0
test -f proj/mise.toml
assert_contains "cat proj/mise.toml" 'python = "3.10.14"'
assert_contains "cat proj/mise.toml" 'six = "1.17.0"'
assert_contains "cat proj/mise.toml" '[packages."python@3.10.14"]'

# attach in a directory with code but no config
mkdir -p code
echo 'print("hello")' >code/main.py
(
  cd code
  mise project attach
  test -f mise.toml
  # code untouched
  assert "cat main.py" 'print("hello")'
)

# ls lists both, status reports the group
mise project ls
assert_contains "mise project ls" "proj"
(cd proj && mise project status)
assert_contains "cd proj && mise project status" "python@3.10.14 six 1.17.0"
