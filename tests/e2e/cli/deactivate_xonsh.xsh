# shellcheck disable=all
$XONSH_SHOW_TRACEBACK = True

# Test that pakro deactivate properly works and can be called multiple times safely
# This tests the fix for https://github.com/jdx/mise/issues/6855

from xonsh.built_ins import XSH

# Create a simple pakro config
config = """
[tools]
tiny = "latest"

[env]
FOO = "pakro_foo"
"""
echo @(config) > .pakro.toml

pakro install

# Activate pakro
# shellcheck disable=SC1073,SC1065,SC1064,SC1072
execx($(pakro activate -s xonsh))
events.on_pre_prompt.fire()

# Verify pakro is activated
assert 'pakro' in aliases, "pakro alias should exist after activation"
assert XSH.env.get('FOO') == 'pakro_foo', "FOO should be set"

# Deactivate pakro
pakro deactivate

# Verify pakro is deactivated
assert 'pakro' not in aliases, "pakro alias should be removed after deactivate"

# Test edge case: deactivating when not activated should be safe
# This is the KEY TEST for the fix - using .pop() instead of del
# means this won't raise KeyError
pakro deactivate
print("deactivate_twice works - fix verified!")

print("All tests passed!")
