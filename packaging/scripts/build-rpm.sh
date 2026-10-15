#!/usr/bin/env bash
set -euxo pipefail

PAKRO_VERSION=$(./packaging/scripts/get-version.sh)

mkdir -p pakro/lib
cat >pakro/lib/pakro-self-update-instructions.toml <<'TOML'
message = "To update pakro from the RPM repository, run:\n\n  sudo dnf upgrade pakro\n"
TOML

tar -xvJf "dist/pakro-$PAKRO_VERSION-linux-x64.tar.xz"
fpm -s dir -t rpm \
	--name pakro \
	--license MIT \
	--version "${PAKRO_VERSION#v*}" \
	--architecture x86_64 \
	--description "Dev tools, env vars, and tasks in one CLI" \
	--url "${PAKRO_PROJECT_URL:-}" \
	--maintainer "${PAKRO_PACKAGE_MAINTAINER:-pakro contributors}" \
	pakro/bin/pakro=/usr/bin/pakro \
	pakro/lib/pakro-self-update-instructions.toml=/usr/lib/pakro/pakro-self-update-instructions.toml \
	pakro/man/man1/pakro.1=/usr/share/man/man1/pakro.1

tar -xvJf "dist/pakro-$PAKRO_VERSION-linux-arm64.tar.xz"
fpm -s dir -t rpm \
	--name pakro \
	--license MIT \
	--version "${PAKRO_VERSION#v*}" \
	--architecture aarch64 \
	--description "Dev tools, env vars, and tasks in one CLI" \
	--url "${PAKRO_PROJECT_URL:-}" \
	--maintainer "${PAKRO_PACKAGE_MAINTAINER:-pakro contributors}" \
	pakro/bin/pakro=/usr/bin/pakro \
	pakro/lib/pakro-self-update-instructions.toml=/usr/lib/pakro/pakro-self-update-instructions.toml \
	pakro/man/man1/pakro.1=/usr/share/man/man1/pakro.1

if [[ -n "${PAKRO_GPG_SIGNING_KEY:-}" ]]; then
cat <<EOF >~/.rpmmacros
%_signature gpg
%_gpg_name $PAKRO_GPG_SIGNING_KEY
EOF
fi

mkdir -p dist/rpmrepo/packages
cp -v packaging/rpm/pakro.repo dist/rpmrepo
cp -v ./*.rpm dist/rpmrepo/packages
if [[ -n "${PAKRO_GPG_SIGNING_KEY:-}" ]]; then
  rpm --addsign dist/rpmrepo/packages/*.rpm
fi
createrepo dist/rpmrepo
if [[ -n "${PAKRO_GPG_SIGNING_KEY:-}" ]]; then
  gpg -u "$PAKRO_GPG_SIGNING_KEY" --batch --yes --detach-sign --armor dist/rpmrepo/repodata/repomd.xml
fi
