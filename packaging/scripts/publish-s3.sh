#!/usr/bin/env -S pakro x aws-cli@2.22.35 -- bash
set -euxo pipefail

source "$(dirname "${BASH_SOURCE[0]}")/release-env.sh"
pakro_require_release

cache_hour="max-age=3600,s-maxage=3600,public"
cache_day="max-age=86400,s-maxage=86400,public,immutable"
cache_week="max-age=604800,s-maxage=604800,public,immutable"

# Configure AWS CLI to use retry mode with exponential backoff
export AWS_RETRY_MODE=adaptive
export AWS_MAX_ATTEMPTS=10

# Upload versioned tarballs to pakro-v{version}/ directory
aws s3 cp "$RELEASE_DIR/$PAKRO_VERSION" "s3://$AWS_S3_BUCKET/$PAKRO_VERSION/" --cache-control "$cache_week" --no-progress --recursive --include "*.tar.gz" --include "*.tar.xz" --include "*.tar.zst" --include "*.zip" --include "SHASUMS*"

which -a aws
aws --version

# Upload pakro-latest-* binaries (excluding archives to avoid conflicts)
# Note: --exclude must come before --include for proper filtering
aws s3 cp "$RELEASE_DIR" "s3://$AWS_S3_BUCKET/" --cache-control "$cache_day" --no-progress --recursive --exclude "*" --include "pakro-latest-*" --exclude "*.tar.gz" --exclude "*.tar.xz" --exclude "*.tar.zst" --exclude "*.zip"

# Upload SHASUMS files
aws s3 cp "$RELEASE_DIR" "s3://$AWS_S3_BUCKET/" --cache-control "$cache_day" --no-progress --content-type "text/plain" --recursive --exclude "*" --include "SHASUMS*"

# The public install.sh and signatures are promoted from the versioned objects
# by publish-version.sh, only after its release index is available.
aws s3 cp "./src/schema/pakro.json" "s3://$AWS_S3_BUCKET/schema/pakro.json" --cache-control "$cache_day" --no-progress --content-type "application/json"
aws s3 cp "./src/schema/pakro.plugin.json" "s3://$AWS_S3_BUCKET/schema/pakro.plugin.json" --cache-control "$cache_day" --no-progress --content-type "application/json"
aws s3 cp "./src/schema/pakro-task.json" "s3://$AWS_S3_BUCKET/schema/pakro-task.json" --cache-control "$cache_day" --no-progress --content-type "application/json"

# Publish only the registry files so older pakro builds can opt into the registry
# tested by this release without downloading the entire source repository. Materialize
# Aqua-derived bins in the archive because older clients only understand explicit bins.
registry_tmpdir="$(mktemp -d)"
trap 'rm -rf "$registry_tmpdir"' EXIT
registry_archive="$registry_tmpdir/registry.tar.zst"
# Keep the public archive rooted at registry/ even though sources now live
# under runtimes/. An older HEAD may still use the original source path.
if git cat-file -e HEAD:runtimes/registry 2>/dev/null; then
	git archive --format=tar HEAD runtimes/registry |
		tar -C "$registry_tmpdir" --strip-components=1 -xf -
else
	git archive --format=tar HEAD registry | tar -C "$registry_tmpdir" -xf -
fi
pakro registry --json --hide-aliased |
	jq -r '.[] | select(.bins | length > 0) | [.short, (.bins | tojson)] | @tsv' |
	while IFS=$'\t' read -r short bins; do
		registry_file="$registry_tmpdir/registry/$short.toml"
		if [[ -f $registry_file ]] && ! grep -q '^bins[[:space:]]*=' "$registry_file"; then
			printf '\nbins = %s\n' "$bins" >>"$registry_file"
		fi
	done
tar -C "$registry_tmpdir" -cf - registry | zstd -q -10 -c >"$registry_archive"
aws s3 cp "$registry_archive" "s3://$AWS_S3_BUCKET/registry/latest.tar.zst" --cache-control "$cache_hour" --no-progress --content-type "application/zstd"

# Upload shell-specific pakro.run scripts
aws s3 cp artifacts/pakro.run/zsh "s3://$AWS_S3_BUCKET/pakro.run/zsh" --cache-control "$cache_week" --no-progress --content-type "text/plain"
aws s3 cp artifacts/pakro.run/bash "s3://$AWS_S3_BUCKET/pakro.run/bash" --cache-control "$cache_week" --no-progress --content-type "text/plain"
aws s3 cp artifacts/pakro.run/fish "s3://$AWS_S3_BUCKET/pakro.run/fish" --cache-control "$cache_week" --no-progress --content-type "text/plain"

aws s3 cp artifacts/rpm/pakro.repo "s3://$AWS_S3_BUCKET/rpm/" --cache-control "$cache_day" --no-progress
aws s3 cp artifacts/rpm/packages/ "s3://$AWS_S3_BUCKET/rpm/packages/" --cache-control "$cache_week" --no-progress --recursive
aws s3 cp artifacts/rpm/repodata/ "s3://$AWS_S3_BUCKET/rpm/repodata/" --cache-control "$cache_day" --no-progress --recursive --exclude "*" --include "repomd.xml*"
aws s3 cp artifacts/rpm/repodata/ "s3://$AWS_S3_BUCKET/rpm/repodata/" --cache-control "$cache_week" --no-progress --recursive --exclude "repomd.xml*"

aws s3 cp artifacts/deb/pool/ "s3://$AWS_S3_BUCKET/deb/pool/" --cache-control "$cache_week" --no-progress --recursive
aws s3 cp artifacts/deb/dists/ "s3://$AWS_S3_BUCKET/deb/dists/" --cache-control "$cache_day" --no-progress --no-progress --recursive

# delete ancient CLIs
# since=`date --date '-3 years' +%F 2>/dev/null`
# aws s3api list-objects-v2 --bucket "$AWS_S3_BUCKET" --query 'Contents[?LastModified < `'"$since"'`]' | jq -r '.[].Key' | grep "^(deb|rpm)\/"

# Cache invalidation happens with VERSION after the GitHub release is public,
# making the new latest artifacts and their discovery pointer visible together.
