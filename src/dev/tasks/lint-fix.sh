#!/usr/bin/env bash
#PAKRO alias=["format", "fix"]
#PAKRO wait_for=["render:schema"]
#PAKRO description="Automatically fix lint issues"
set -euxo pipefail

hk fix --all --exclude runtimes/vendor/aqua-registry
