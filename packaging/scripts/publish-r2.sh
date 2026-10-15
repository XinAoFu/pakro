#!/usr/bin/env bash
set -euxo pipefail

source "$(dirname "${BASH_SOURCE[0]}")/release-env.sh"
pakro_require_release

export AWS_REGION=auto
export AWS_DEFAULT_OUTPUT=json
export AWS_ENDPOINT_URL="${PAKRO_R2_ENDPOINT_URL:?Set PAKRO_R2_ENDPOINT_URL}"
export AWS_ACCESS_KEY_ID=$CLOUDFLARE_ACCESS_KEY_ID
export AWS_SECRET_ACCESS_KEY=$CLOUDFLARE_SECRET_ACCESS_KEY
export AWS_S3_BUCKET="${PAKRO_RELEASE_BUCKET:?Set PAKRO_RELEASE_BUCKET}"

./packaging/scripts/publish-s3.sh
