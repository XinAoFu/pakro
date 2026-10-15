#!/usr/bin/env sh

pakro_bin="$1"
shell_type="$2"

for _ in 1 2 3; do
  eval "$("$pakro_bin" activate "$shell_type" --shims)"
done

printf '%s\n' "$PATH"
