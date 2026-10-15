#!/usr/bin/env bash
set -euxo pipefail

source "$(dirname "${BASH_SOURCE[0]}")/release-env.sh"
pakro_require_release

if [ -z "${CLOUDFLARE_API_TOKEN:-}" ]; then
	echo "Error: CLOUDFLARE_API_TOKEN environment variable is required"
	exit 1
fi

ACCOUNT_ID="${PAKRO_CLOUDFLARE_ACCOUNT_ID:?Set PAKRO_CLOUDFLARE_ACCOUNT_ID}"
WORKER_NAME="${PAKRO_CLOUDFLARE_WORKER_NAME:-pakro-run}"

echo "Deploying updated worker code for pakro.run to worker: $WORKER_NAME"

if [[ $DRY_RUN != 1 ]]; then
  : "${PAKRO_DOWNLOAD_BASE_URL:?Set PAKRO_DOWNLOAD_BASE_URL}"
  worker_script=$(mktemp)
  trap 'rm -f "$worker_script"' EXIT
  jq -Rs --arg url "$PAKRO_DOWNLOAD_BASE_URL" \
    'gsub("\"__PAKRO_DOWNLOAD_BASE_URL__\""; $url | tojson)' \
    < packaging/cloudflare/workers/pakro-run.js > "$worker_script"
	# Upload the worker script
	response=$(curl -s -X PUT "https://api.cloudflare.com/client/v4/accounts/$ACCOUNT_ID/workers/scripts/$WORKER_NAME/content" \
		-H "Authorization: Bearer $CLOUDFLARE_API_TOKEN" \
		-H "Content-Type: application/javascript" \
		--data-binary "@$worker_script")

	if echo "$response" | jq -e '.success == true' >/dev/null; then
		echo "✅ Worker deployed successfully!"
	else
		echo "❌ Worker deployment failed:"
		echo "$response" | jq .
		exit 1
	fi
fi

# Show current routes
echo ""
echo "Current worker routes:"
curl -s -X GET "https://api.cloudflare.com/client/v4/accounts/$ACCOUNT_ID/workers/scripts/$WORKER_NAME/routes" \
	-H "Authorization: Bearer $CLOUDFLARE_API_TOKEN" \
	-H "Content-Type: application/json" | jq -r '.result[]?.pattern // "No routes found"'
