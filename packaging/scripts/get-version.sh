#!/bin/sh
set -eu

pakro_release_repo_root="$(CDPATH='' cd -- "$(dirname -- "$0")/../.." && pwd)"
echo "v$(grep '^version =' "$pakro_release_repo_root/Cargo.toml" | head -n1 | cut -d '"' -f 2)"
