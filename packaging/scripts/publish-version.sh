#!/usr/bin/env bash
set -euo pipefail

source "$(dirname "${BASH_SOURCE[0]}")/release-env.sh"
pakro_require_release

# shellcheck source=packaging/scripts/gh-api-retry.sh
source "$(dirname "${BASH_SOURCE[0]}")/gh-api-retry.sh"

TAG=${1:?usage: publish-version.sh <release-tag>}
if ! [[ $TAG =~ ^v[0-9][0-9A-Za-z._+-]*$ ]]; then
	echo "Invalid release tag: $TAG" >&2
	exit 1
fi

version_file=$(mktemp)
release_index=$(mktemp)
trap 'rm -f "$version_file" "$release_index"' EXIT
printf '%s\n' "${TAG#v}" >"$version_file"

export AWS_REGION=auto
export AWS_DEFAULT_OUTPUT=json
export AWS_ENDPOINT_URL="${PAKRO_R2_ENDPOINT_URL:?Set PAKRO_R2_ENDPOINT_URL}"
export AWS_ACCESS_KEY_ID=$CLOUDFLARE_ACCESS_KEY_ID
export AWS_SECRET_ACCESS_KEY=$CLOUDFLARE_SECRET_ACCESS_KEY
export AWS_RETRY_MODE=adaptive
export AWS_MAX_ATTEMPTS=10
release_bucket="${PAKRO_RELEASE_BUCKET:?Set PAKRO_RELEASE_BUCKET}"

# A recovery run can publish a draft without rerunning the original CDN staging
# step. Do not expose it as latest unless every archive used by installers is
# already present in R2.
unix_platforms=(
	linux-x64
	linux-x64-musl
	linux-arm64
	linux-arm64-musl
	linux-armv7
	linux-armv7-musl
	macos-x64
	macos-arm64
)
for platform in "${unix_platforms[@]}"; do
	for extension in tar.gz tar.xz tar.zst; do
		aws s3api head-object \
			--bucket "$release_bucket" \
			--key "$TAG/pakro-$TAG-$platform.$extension" >/dev/null
	done
done
aws s3api head-object --bucket "$release_bucket" --key "$TAG/SHASUMS256.txt" >/dev/null

for installer in install.sh install.sh.sig install.sh.minisig; do
	aws s3api head-object --bucket "$release_bucket" --key "$TAG/$installer" >/dev/null
done

# Publish the complete stable release history after GitHub publication. Consumers
# apply their cutoff at runtime, so aging into eligibility needs no deployment.
gh_api --paginate "repos/$PAKRO_RELEASE_REPOSITORY/releases?per_page=100" |
	jq -sr '
		add | map(select(.draft == false and .prerelease == false)) |
		map(select(.tag_name | test("^v[0-9]+\\.[0-9]+\\.[0-9]+$"))) |
		if length == 0 then error("no stable pakro releases") else . end |
		sort_by(.tag_name | ltrimstr("v") | split(".") | map(tonumber)) | reverse |
		.[] | [.tag_name, (.published_at | fromdateiso8601)] | @tsv
	' >"$release_index"
awk -v tag="$TAG" '$1 == tag { found = 1 } END { exit !found }' "$release_index"
aws s3 cp "$release_index" "s3://$release_bucket/releases.tsv" \
	--cache-control "max-age=300,s-maxage=300,public" \
	--no-progress --content-type "text/plain"

# Promote the installer only after publishing the index it requires. The release
# staging job uploads these files under the tag without changing the public URL.
for installer in install.sh install.sh.sig install.sh.minisig; do
	aws s3 cp "s3://$release_bucket/$TAG/$installer" "s3://$release_bucket/$installer" \
		--cache-control "max-age=86400,s-maxage=86400,public,immutable" \
		--no-progress --content-type "text/plain"
done

aws s3 cp "$version_file" "s3://$release_bucket/VERSION" \
	--cache-control "max-age=86400,s-maxage=86400,public,immutable" \
	--no-progress \
	--content-type "text/plain"

# Purge every zone that fronts release artifacts only after VERSION points at
# the public release. This also makes the already-uploaded latest assets and
# install scripts visible in the same post-publication cache refresh.
read -r -a ZONES <<< "${PAKRO_CLOUDFLARE_ZONES:-}"
for entry in "${ZONES[@]}"; do
	IFS=":" read -r HOST ZONE_ID <<<"$entry"
	echo "Purging CDN cache for $HOST (zone=$ZONE_ID)"
	curl --fail-with-body -X POST "https://api.cloudflare.com/client/v4/zones/$ZONE_ID/purge_cache" \
		-H "Authorization: Bearer $CLOUDFLARE_API_TOKEN" \
		-H "Content-Type: application/json" \
		--data '{ "purge_everything": true }'
done
