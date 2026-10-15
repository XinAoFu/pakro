#!/usr/bin/env bash
set -euxo pipefail

PAKRO_VERSION=$(./packaging/scripts/get-version.sh)

mkdir -p pakro/lib
cat >pakro/lib/pakro-self-update-instructions.toml <<'TOML'
message = "To update pakro from the APT repository, run:\n\n  sudo apt update && sudo apt install --only-upgrade pakro\n"
TOML

tar -xvJf "dist/pakro-$PAKRO_VERSION-linux-x64.tar.xz"
fpm -s dir -t deb \
	--name pakro \
	--license MIT \
	--version "${PAKRO_VERSION#v*}" \
	--architecture amd64 \
	--description "Dev tools, env vars, and tasks in one CLI" \
	--url "${PAKRO_PROJECT_URL:-}" \
	--maintainer "${PAKRO_PACKAGE_MAINTAINER:-pakro contributors}" \
	pakro/bin/pakro=/usr/bin/pakro \
	pakro/lib/pakro-self-update-instructions.toml=/usr/lib/pakro/pakro-self-update-instructions.toml \
	pakro/man/man1/pakro.1=/usr/share/man/man1/pakro.1

tar -xvJf "dist/pakro-$PAKRO_VERSION-linux-arm64.tar.xz"
fpm -s dir -t deb \
	--name pakro \
	--license MIT \
	--version "${PAKRO_VERSION#v*}" \
	--architecture arm64 \
	--description "Dev tools, env vars, and tasks in one CLI" \
	--url "${PAKRO_PROJECT_URL:-}" \
	--maintainer "${PAKRO_PACKAGE_MAINTAINER:-pakro contributors}" \
	pakro/bin/pakro=/usr/bin/pakro \
	pakro/lib/pakro-self-update-instructions.toml=/usr/lib/pakro/pakro-self-update-instructions.toml \
	pakro/man/man1/pakro.1=/usr/share/man/man1/pakro.1

mkdir -p dist/deb/pool/main
cp -v ./*.deb dist/deb/pool/main
mkdir -p dist/deb/dists/stable/main/binary-amd64
mkdir -p dist/deb/dists/stable/main/binary-arm64
cd dist/deb
dpkg-scanpackages --arch amd64 pool/ >dists/stable/main/binary-amd64/Packages
dpkg-scanpackages --arch arm64 pool/ >dists/stable/main/binary-arm64/Packages
gzip -9c <dists/stable/main/binary-amd64/Packages >dists/stable/main/binary-amd64/Packages.gz
gzip -9c <dists/stable/main/binary-arm64/Packages >dists/stable/main/binary-arm64/Packages.gz
cd ../..

cd dist/deb/dists/stable
"$GITHUB_WORKSPACE/packaging/deb/generate-release.sh" >Release
if [[ -n "${PAKRO_GPG_SIGNING_KEY:-}" ]]; then
  gpg -u "$PAKRO_GPG_SIGNING_KEY" -abs <Release >Release.gpg
  gpg -u "$PAKRO_GPG_SIGNING_KEY" -abs --clearsign <Release >InRelease
fi
cd "$GITHUB_WORKSPACE"
