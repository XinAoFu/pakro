#!/usr/bin/env bash
# Shared release configuration. Local archive/package builds do not enable publishing.
pakro_require_repository() {
  PAKRO_RELEASE_REPOSITORY="${PAKRO_RELEASE_REPOSITORY:-${GITHUB_REPOSITORY:-}}"
  if [[ ! "$PAKRO_RELEASE_REPOSITORY" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ || "$PAKRO_RELEASE_REPOSITORY" == "jdx/mise" ]]; then
    echo "Set PAKRO_RELEASE_REPOSITORY to the pakro release repository." >&2
    return 1
  fi
  export PAKRO_RELEASE_REPOSITORY
}

pakro_require_release() {
  if [[ "${PAKRO_ENABLE_RELEASE:-false}" != "true" ]]; then
    echo "Pakro publishing is disabled. Configure its release targets and set PAKRO_ENABLE_RELEASE=true." >&2
    return 1
  fi
  pakro_require_repository
}
