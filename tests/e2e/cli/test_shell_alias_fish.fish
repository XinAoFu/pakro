#!/usr/bin/env fish

# Test fish shell alias completions are properly managed

# Test 1: Shell alias set includes complete -e to clear stale completions
printf '[shell_alias]\nll = "ls -la"\n' >pakro.toml

set -e __PAKRO_SESSION
set -e __PAKRO_DIFF
set output (pakro hook-env -s fish --force)
string match -q '*complete -e ll*' -- $output; or begin; echo "FAIL: expected 'complete -e ll' in set_alias output"; exit 1; end
string match -q '*alias ll*' -- $output; or begin; echo "FAIL: expected 'alias ll' in set_alias output"; exit 1; end

# Establish session
pakro hook-env -s fish --force | source

# Test 2: Alias removal includes complete -e to clean up leaked completions
printf '# empty config\n' >pakro.toml

set output (pakro hook-env -s fish --force)
string match -q '*complete -e ll*' -- $output; or begin; echo "FAIL: expected 'complete -e ll' in unset_alias output"; exit 1; end
string match -q '*functions -e ll*' -- $output; or begin; echo "FAIL: expected 'functions -e ll' in unset_alias output"; exit 1; end
