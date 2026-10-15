#!/usr/bin/env fish

mkdir project-one project-two
printf '[env]\nPAKRO_FISH_CD_TEST = "one"\n' >project-one/pakro.toml
printf '[env]\nPAKRO_FISH_CD_TEST = "two"\n' >project-two/pakro.toml

pakro activate fish | source

# Reproduce an interactive prompt followed by a separate command.
emit fish_prompt
emit fish_preexec 'cd project-one; cd ../project-two'

functions -q __pakro_cd_hook
or begin
    echo "expected the default PWD hook to remain active during command execution"
    exit 1
end

cd project-one
test "$PAKRO_FISH_CD_TEST" = one
or begin
    echo "expected the first directory change to update the environment"
    exit 1
end

cd ../project-two
test "$PAKRO_FISH_CD_TEST" = two
or begin
    echo "expected the second directory change to update the environment"
    exit 1
end

cd ..
pakro deactivate

set -g pakro_fish_mode eval_after_arrow
pakro activate fish | source
emit fish_prompt
emit fish_preexec 'cd project-one'

functions -q __pakro_cd_hook
and begin
    echo "expected eval_after_arrow to retain its delayed PWD hook behavior"
    exit 1
end

cd project-one
set -q PAKRO_FISH_CD_TEST
and begin
    echo "expected eval_after_arrow to defer the environment update until the prompt"
    exit 1
end

emit fish_prompt
test "$PAKRO_FISH_CD_TEST" = one
or begin
    echo "expected the prompt to apply the deferred environment update"
    exit 1
end

pakro deactivate
