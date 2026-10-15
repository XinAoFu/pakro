#!/usr/bin/env fish

set -g pakro_fish_mode disable_arrow
pakro activate fish | source

set -g pakro_env_eval_count 0
function __pakro_env_eval
    set -g pakro_env_eval_count (math $pakro_env_eval_count + 1)
end

set -l activate_pwd $PWD
cd ..
__pakro_env_eval_on_prompt
test $pakro_env_eval_count -eq 1
or begin
    echo "expected a directory change before the first fish_prompt event to run hook-env"
    exit 1
end
cd $activate_pwd

pakro deactivate
pakro activate fish | source

set -g pakro_env_eval_count 0
function __pakro_env_eval
    set -g pakro_env_eval_count (math $pakro_env_eval_count + 1)
end

__pakro_env_eval_on_prompt
test $pakro_env_eval_count -eq 0
or begin
    echo "expected the first fish_prompt event to skip hook-env"
    exit 1
end

__pakro_env_eval_on_prompt
test $pakro_env_eval_count -eq 1
or begin
    echo "expected the second fish_prompt event to run hook-env"
    exit 1
end

pakro deactivate
