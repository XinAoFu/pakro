#!/usr/bin/env bash
set -euxo pipefail

source "$(dirname "${BASH_SOURCE[0]}")/release-env.sh"
pakro_require_release
: "${PAKRO_GPG_SIGNING_KEY:?Set the pakro GPG signing key}"
: "${PAKRO_MINISIGN_PUBLIC_KEY_PATH:?Set the pakro minisign public key path}"
: "${PAKRO_ZIPSIGN_PUBLIC_KEY_PATH:?Set the pakro zipsign public key path}"

echo "::group::Setup"
git config user.name "${PAKRO_RELEASE_NAME:-pakro Release Bot}"
git config user.email "${PAKRO_RELEASE_EMAIL:?Set PAKRO_RELEASE_EMAIL}"

BASE_DIR="$(pwd)"
PAKRO_VERSION=$(./packaging/scripts/get-version.sh)
RELEASE_DIR=releases
MINISIGN_SECRET_KEY_PATH="${MINISIGN_SECRET_KEY_PATH:-$BASE_DIR/packaging/resources/keys/minisign.key}"
export BASE_DIR PAKRO_VERSION RELEASE_DIR
rm -rf "${RELEASE_DIR:?}/$PAKRO_VERSION"
mkdir -p "$RELEASE_DIR/$PAKRO_VERSION"

echo "::group::Build"
platforms=(
	linux-x64
	linux-x64-musl
	linux-arm64
	linux-arm64-musl
	linux-armv7
	linux-armv7-musl
	macos-x64
	macos-arm64
)
for platform in "${platforms[@]}"; do
	cp artifacts/*/"pakro-$PAKRO_VERSION-$platform.tar.gz" "$RELEASE_DIR/$PAKRO_VERSION/pakro-$PAKRO_VERSION-$platform.tar.gz"
	cp artifacts/*/"pakro-$PAKRO_VERSION-$platform.tar.xz" "$RELEASE_DIR/$PAKRO_VERSION/pakro-$PAKRO_VERSION-$platform.tar.xz"
	cp artifacts/*/"pakro-$PAKRO_VERSION-$platform.tar.zst" "$RELEASE_DIR/$PAKRO_VERSION/pakro-$PAKRO_VERSION-$platform.tar.zst"
	zipsign sign tar "$RELEASE_DIR/$PAKRO_VERSION/pakro-$PAKRO_VERSION-$platform.tar.gz" ~/.zipsign/pakro.priv
	zipsign verify tar "$RELEASE_DIR/$PAKRO_VERSION/pakro-$PAKRO_VERSION-$platform.tar.gz" "$PAKRO_ZIPSIGN_PUBLIC_KEY_PATH"
	cp "$RELEASE_DIR/$PAKRO_VERSION/pakro-$PAKRO_VERSION-$platform.tar.gz" "$RELEASE_DIR/pakro-latest-$platform.tar.gz"
	cp "$RELEASE_DIR/$PAKRO_VERSION/pakro-$PAKRO_VERSION-$platform.tar.xz" "$RELEASE_DIR/pakro-latest-$platform.tar.xz"
	cp "$RELEASE_DIR/$PAKRO_VERSION/pakro-$PAKRO_VERSION-$platform.tar.zst" "$RELEASE_DIR/pakro-latest-$platform.tar.zst"
	tar -xvzf "$RELEASE_DIR/$PAKRO_VERSION/pakro-$PAKRO_VERSION-$platform.tar.gz"
	cp -v pakro/bin/pakro "$RELEASE_DIR/pakro-latest-$platform"
	cp -v pakro/bin/pakro "$RELEASE_DIR/$PAKRO_VERSION/pakro-$PAKRO_VERSION-$platform"
done

echo "::group::Checksums"
pushd "$RELEASE_DIR"
cp pakro-latest-linux-x64 pakro-latest-linux-amd64
cp pakro-latest-macos-x64 pakro-latest-macos-amd64
sha256sum ./pakro-latest-* >SHASUMS256.txt
sha512sum ./pakro-latest-* >SHASUMS512.txt
gpg --clearsign -u "$PAKRO_GPG_SIGNING_KEY" <SHASUMS256.txt >SHASUMS256.asc
gpg --clearsign -u "$PAKRO_GPG_SIGNING_KEY" <SHASUMS512.txt >SHASUMS512.asc
minisign -WSs "$MINISIGN_SECRET_KEY_PATH" -p "$PAKRO_MINISIGN_PUBLIC_KEY_PATH" -m SHASUMS256.txt SHASUMS512.txt </dev/zero
popd

pushd "$RELEASE_DIR/$PAKRO_VERSION"
sha256sum ./* >SHASUMS256.txt
sha512sum ./* >SHASUMS512.txt
gpg --clearsign -u "$PAKRO_GPG_SIGNING_KEY" <SHASUMS256.txt >SHASUMS256.asc
gpg --clearsign -u "$PAKRO_GPG_SIGNING_KEY" <SHASUMS512.txt >SHASUMS512.asc
minisign -WSs "$MINISIGN_SECRET_KEY_PATH" -p "$PAKRO_MINISIGN_PUBLIC_KEY_PATH" -m SHASUMS256.txt SHASUMS512.txt </dev/zero
popd

echo "::group::install.sh"
./packaging/scripts/render-install.sh >"$RELEASE_DIR"/install.sh
chmod +x "$RELEASE_DIR"/install.sh
shellcheck "$RELEASE_DIR"/install.sh
gpg -u "$PAKRO_GPG_SIGNING_KEY" --output "$RELEASE_DIR"/install.sh.sig --sign "$RELEASE_DIR"/install.sh
minisign -WSs "$MINISIGN_SECRET_KEY_PATH" -p "$PAKRO_MINISIGN_PUBLIC_KEY_PATH" -m "$RELEASE_DIR"/install.sh </dev/zero
cp "$RELEASE_DIR"/{install.sh,install.sh.sig,install.sh.minisig} "$RELEASE_DIR/$PAKRO_VERSION"

echo "::group::pakro.run scripts"
./packaging/scripts/render-pakro-run.sh

echo "::group::Sign source tarball"
TMP_FILE="$(mktemp)"
curl -L -o "$TMP_FILE" "https://github.com/$PAKRO_RELEASE_REPOSITORY/archive/refs/tags/$PAKRO_VERSION.tar.gz"
gpg --detach-sign -u "$PAKRO_GPG_SIGNING_KEY" <"$TMP_FILE" >"$RELEASE_DIR/$PAKRO_VERSION/$PAKRO_VERSION.tar.gz.sig"
rm "$TMP_FILE"

# Publishing is now done after GitHub release is created
