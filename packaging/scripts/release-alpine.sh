#!/usr/bin/env bash
set -euxo pipefail

source "$(dirname "${BASH_SOURCE[0]}")/release-env.sh"
pakro_require_release

PAKRO_VERSION=$(./packaging/scripts/get-latest-version.sh)

export GITLAB_HOST=gitlab.alpinelinux.org
export GITLAB_TOKEN="$ALPINE_GITLAB_TOKEN"
# GitHub repo (a fork of alpinelinux/aports) that receives a copy of each bump
ALPINE_BACKUP_REPO="${ALPINE_BACKUP_REPO:?Set the pakro aports backup repository}"
: "${ALPINE_GITLAB_FORK:?Set the pakro GitLab aports fork}"

sudo chown -R packager:packager /github/home
mkdir -p /github/home/.abuild
echo "$ALPINE_PUB_KEY" | sudo tee "/etc/apk/keys/$ALPINE_KEY_ID.pub"
echo "$ALPINE_PUB_KEY" >"/github/home/.abuild/$ALPINE_KEY_ID.pub"
echo "$ALPINE_PRIV_KEY" >"/github/home/.abuild/$ALPINE_KEY_ID"
echo "PACKAGER_PRIVKEY=\"/github/home/.abuild/$ALPINE_KEY_ID\"" >>/github/home/.abuild/abuild.conf


# gitlab.alpinelinux.org answers git requests from CI IP ranges with an HTTP 418
# anti-bot challenge, with or without credentials, so clone the GitHub mirror
# instead. It tracks the same history; we still push and open the MR on GitLab.
git clone https://github.com/alpinelinux/aports.git /home/packager/aports
cd /home/packager/aports
git config user.name "${PAKRO_RELEASE_NAME:-pakro Release Bot}"
git config user.email "${PAKRO_RELEASE_EMAIL:?Set PAKRO_RELEASE_EMAIL}"
git config --local core.hooksPath .githooks
if [ -n "${ALPINE_GITLAB_SSH_KEY:-}" ]; then
	# The 418 challenge is served by the HTTP proxy in front of GitLab; sshd is
	# not behind it, so pushing over SSH avoids it. Optional until the deploy key
	# secret exists, then preferred over HTTPS.
	sudo apk add --no-cache openssh-client
	mkdir -p /home/packager/.ssh
	chmod 700 /home/packager/.ssh
	# keep the key out of the xtrace output
	{ set +x; } 2>/dev/null
	echo "$ALPINE_GITLAB_SSH_KEY" >/home/packager/.ssh/gitlab_alpine
	set -x
	chmod 600 /home/packager/.ssh/gitlab_alpine
	echo "gitlab.alpinelinux.org ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIA3AxnPM+Cquq/2QXWWmuHdgmfMIU+v/7kV/k3p+KpBd" >/home/packager/.ssh/known_hosts
	export GIT_SSH_COMMAND="ssh -i /home/packager/.ssh/gitlab_alpine -o IdentitiesOnly=yes -o UserKnownHostsFile=/home/packager/.ssh/known_hosts -o StrictHostKeyChecking=yes"
	git remote add pakro "git@gitlab.alpinelinux.org:$ALPINE_GITLAB_FORK.git"
else
	git remote add pakro "https://oauth2:$GITLAB_TOKEN@gitlab.alpinelinux.org/$ALPINE_GITLAB_FORK.git/"
fi
git checkout -mb pakro
test -f community/pakro/APKBUILD || { echo "Add the pakro APKBUILD to the configured aports fork before releasing." >&2; exit 1; }
cd community/pakro

sed -i "s/pkgver=.*/pkgver=${PAKRO_VERSION#v}/" APKBUILD

abuild checksum
cat /github/home/.abuild/abuild.conf
abuild -r
#apkbuild-lint APKBUILD fails due to: SC:[AL57]:./APKBUILD:7:invalid arch '!loongarch64'

git add APKBUILD

if git diff --cached --exit-code; then
	echo "No changes to commit"
	exit 0
fi
git commit -m "community/pakro: upgrade to ${PAKRO_VERSION#v}"

if [ "$DRY_RUN" == 0 ]; then
	# Keep a copy of the bump on GitHub first. gitlab.alpinelinux.org may answer
	# the push below with an HTTP 418 anti-bot challenge from CI IP ranges; the
	# build takes ~40 minutes, so this lets someone finish the push from a
	# machine GitLab accepts instead of re-running the whole job. Best effort:
	# a failure here must not block the real push.
	{ set +x; } 2>/dev/null
	git remote add backup "https://x-access-token:$GH_TOKEN@github.com/$ALPINE_BACKUP_REPO.git"
	set -x
	backup_branch="pakro-${PAKRO_VERSION#v}"
	backup_ok=0
	if git push backup "HEAD:refs/heads/$backup_branch" -f; then
		backup_ok=1
	else
		echo "::warning::could not push $backup_branch to github.com/$ALPINE_BACKUP_REPO"
	fi

	pushed=0
	for attempt in 1 2 3; do
		if git push pakro -f; then
			pushed=1
			break
		fi
		echo "git push to gitlab.alpinelinux.org failed (attempt $attempt/3)"
		if [ "$attempt" -lt 3 ]; then
			sleep $((attempt * 60))
		fi
	done
	if [ "$pushed" == 0 ]; then
		echo "::error::could not push to gitlab.alpinelinux.org/$ALPINE_GITLAB_FORK (likely an HTTP 418 bot challenge on CI IPs)."
		if [ "$backup_ok" == 1 ]; then
			cat <<EOF
The bump is committed at github.com/$ALPINE_BACKUP_REPO branch $backup_branch. To finish by hand:
  git clone --branch $backup_branch --single-branch https://github.com/$ALPINE_BACKUP_REPO.git aports
  cd aports && git checkout -b pakro
  git push git@gitlab.alpinelinux.org:$ALPINE_GITLAB_FORK.git pakro -f
  GITLAB_HOST=gitlab.alpinelinux.org glab mr create --fill --yes -H $ALPINE_GITLAB_FORK -R alpine/aports
EOF
		else
			cat <<EOF
The GitHub backup push also failed, so the commit only existed in this job. Re-run the job, or
bump community/pakro/APKBUILD (pkgver, then abuild checksum) by hand and push it to $ALPINE_GITLAB_FORK.
EOF
		fi
		exit 1
	fi
fi

open_mr="$(glab mr list -R alpine/aports --author=@me)"
if [[ $open_mr != "Showing"* ]]; then
	if [ "$DRY_RUN" == 0 ]; then
		DEBUG=1 glab mr create --fill --yes -H $ALPINE_GITLAB_FORK -R alpine/aports
	fi
fi
#git show
