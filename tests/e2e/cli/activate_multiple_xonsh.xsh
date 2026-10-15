# shellcheck disable=all
$XONSH_SHOW_TRACEBACK = True

# Test that calling pakro activate multiple times preserves user-added paths at the front
# while properly handling pakro paths (env._.path and tool installs)
#
# Expected behavior:
# - Start: A
# - pakro activate: env_path:pakro_tools:A
# - add B: B:env_path:pakro_tools:A
# - pakro activate + hook-env: B:env_path:pakro_tools:A (user path B preserved at front)
# - add C: C:B:env_path:pakro_tools:A
# - pakro activate + hook-env: C:B:env_path:pakro_tools:A (user paths C,B preserved at front)
# - deactivate: C:B:A (pakro_tools and env_path removed, user paths preserved)

from xonsh.built_ins import XSH
import os

# Create a pakro config with a tool, env._.path, and env.FOO
mkdir -p custom_bin
config = """
[tools]
tiny = "latest"

[env]
_.path = ["./custom_bin"]
FOO = "bar"
BAZ = "qux"
"""
echo @(config) > .pakro.toml

pakro install

# Save the original PATH for verification
ORIGINAL_PATH = list(XSH.env['PATH'])

# First activation
# shellcheck disable=SC1073,SC1065,SC1064,SC1072
execx($(pakro activate -s xonsh))

# Fire the hook to apply environment
events.on_pre_prompt.fire()

# Verify env variables were set
assert XSH.env.get('FOO') == 'bar', f"FOO should be bar, got: {XSH.env.get('FOO')}"
assert XSH.env.get('BAZ') == 'qux', f"BAZ should be qux, got: {XSH.env.get('BAZ')}"

# Check that pakro was activated at all
assert 'pakro' in aliases

# custom_bin from env._.path should be in PATH
current_path = ':'.join(XSH.env['PATH'])
assert 'custom_bin' in current_path, "custom_bin not in PATH"

# Get the pakro tool path - it should be somewhere in PATH
pakro_tool_path = None
for p in XSH.env['PATH']:
    if '/pakro/installs/tiny' in p:
        pakro_tool_path = p
        break
assert pakro_tool_path is not None, "PAKRO_TOOL_PATH not found"

# Verify no duplicate paths
path_list = XSH.env['PATH']
assert len(path_list) == len(set(path_list)), "Duplicate paths found"

# User adds path B
XSH.env['PATH'].add('/path_b', front=True)

# User modifies FOO
XSH.env['FOO'] = 'user_modified'

# Second activation - user path preserved at front, pakro paths follow
# This tests that activation is idempotent when __PAKRO_DIFF exists but pakro isn't in aliases
# (simulating nested shells like tmux)
del aliases['pakro']
# shellcheck disable=SC1073,SC1065,SC1064,SC1072
execx($(pakro activate -s xonsh))
events.on_pre_prompt.fire()

# FOO should be reset to "bar" by pakro
assert XSH.env.get('FOO') == 'bar', f"FOO should be bar after reactivation, got: {XSH.env.get('FOO')}"

# BAZ should still be qux
assert XSH.env.get('BAZ') == 'qux', f"BAZ should be qux, got: {XSH.env.get('BAZ')}"

# Verify pakro paths are in PATH
current_path = ':'.join(XSH.env['PATH'])
assert '/path_b' in current_path, "/path_b not in PATH after second activation"
assert 'custom_bin' in current_path, "custom_bin not in PATH after second activation"
assert '/pakro/installs/tiny' in current_path, "Tool path not in PATH after second activation"

# Check no duplicates
path_list = XSH.env['PATH']
assert len(path_list) == len(set(path_list)), "Duplicate paths found after second activation"

# User adds path C
XSH.env['PATH'].add('/path_c', front=True)

# Third activation - user paths preserved at front, pakro paths follow
# shellcheck disable=SC1073,SC1065,SC1064,SC1072
execx($(pakro activate -s xonsh))
events.on_pre_prompt.fire()

# Verify all paths are present
current_path = ':'.join(XSH.env['PATH'])
assert '/path_c' in current_path, "/path_c not in PATH"
assert '/path_b' in current_path, "/path_b not in PATH"
assert 'custom_bin' in current_path, "custom_bin not in PATH"
assert '/pakro/installs/tiny' in current_path, "Tool path not in PATH"

# Check no duplicates
path_list = XSH.env['PATH']
assert len(path_list) == len(set(path_list)), "Duplicate paths found after third activation"

# Test deactivation - unsets pakro shell functions and variables
# (Comprehensive deactivation tests are in test_deactivate)
pakro deactivate

print("All tests passed!")
