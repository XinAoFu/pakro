#!/usr/bin/env fish
# shellcheck disable=SC1072,SC1065,SC1064,SC1073,SC2103

set -l fish_trace 1
pakro install tiny@3.1.0 tiny@2.0.0
or exit

echo >.pakro.toml '
[tools]
tiny = "3.1.0"
[env]
FOO = "bar"
'

mkdir subdir
echo >subdir/.pakro.toml '
[tools]
tiny = "2.0.0"
[env]
FOO = "quz"
'

pakro activate --status fish | source
__pakro_env_eval

rtx-tiny | grep "v3.1.0"
or exit

cd subdir && __pakro_env_eval
rtx-tiny | grep "v2.0.0"
or exit

cd .. && __pakro_env_eval
rtx-tiny | grep "v3.1.0"
or exit

pakro shell tiny@3.0.0 && __pakro_env_eval
rtx-tiny | grep "v3.0.0"
or exit

pakro deactivate
