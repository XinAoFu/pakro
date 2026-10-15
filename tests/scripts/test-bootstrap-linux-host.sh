#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
pakro_bin=${1:-$repo_root/target/debug/pakro}

if [[ ! -x $pakro_bin ]]; then
	echo "pakro binary is not executable: $pakro_bin" >&2
	exit 1
fi
pakro_bin=$(cd "$(dirname "$pakro_bin")" && pwd)/$(basename "$pakro_bin")
for command in docker ssh ssh-keygen; do
	if ! command -v "$command" >/dev/null; then
		echo "required command is unavailable: $command" >&2
		exit 1
	fi
done

test_tmp_root=${RUNNER_TEMP:-/tmp}
case_dir=$(mktemp -d "$test_tmp_root/pakro-bootstrap-host.XXXXXXXX")
container="pakro-bootstrap-host-$RANDOM-$$"
image="pakro-bootstrap-host:test-$RANDOM-$$"

cleanup() {
	status=$?
	if ((status != 0)); then
		docker logs "$container" >&2 2>/dev/null || true
		docker exec "$container" systemctl --failed --no-pager >&2 2>/dev/null || true
	fi
	docker rm --force "$container" >/dev/null 2>&1 || true
	docker image rm --force "$image" >/dev/null 2>&1 || true
	case "$case_dir" in
	"$test_tmp_root"/pakro-bootstrap-host.*) rm -rf -- "$case_dir" ;;
	*) echo "refusing to remove unexpected test directory: $case_dir" >&2 ;;
	esac
	return "$status"
}
trap cleanup EXIT

ssh-keygen -q -t ed25519 -N '' -f "$case_dir/id_ed25519"

cat >"$case_dir/pakro.toml" <<'TOML'
[bootstrap.groups.pakro-case]
system = true

[bootstrap.users.pakro-case]
system = true
group = "pakro-case"
home = "/var/lib/pakro-case"
shell = "/usr/sbin/nologin"
comment = "pakro bootstrap host acceptance test"
create_home = true

[bootstrap.directories."/opt/pakro-case"]
owner = "pakro-case"
group = "pakro-case"
mode = "0750"

[bootstrap.files."/etc/pakro-case.conf"]
content = "generation=1\n"
owner = "root"
group = "root"
mode = "0640"
notify = ["pakro-case"]

[bootstrap.files."/etc/systemd/system/pakro-case.service"]
content = '''
[Unit]
Description=pakro bootstrap host acceptance service

[Service]
ExecStart=/usr/bin/tail -f /dev/null

[Install]
WantedBy=multi-user.target
'''
owner = "root"
group = "root"
mode = "0644"
notify = ["pakro-case"]

[bootstrap.files."/opt/pakro-case/compose.yaml"]
source = "./compose.yaml"
owner = "pakro-case"
group = "pakro-case"
mode = "0640"

[bootstrap.services.docker]
state = "running"
enabled = true

[bootstrap.services.pakro-case]
state = "running"
enabled = true
on_change = "restart"

[bootstrap.services.pakro-case-user]
scope = "user"
command = "/bin/sleep 300"
description = "pakro bootstrap user-service smoke"

[bootstrap.linux.firewall]
backend = "nftables"
state = "enabled"
default_incoming = "deny"
default_outgoing = "allow"

[[bootstrap.linux.firewall.rules]]
name = "ssh"
direction = "incoming"
action = "allow"
port = 22
protocol = "tcp"

[bootstrap.compose.smoke]
project_dir = "/opt/pakro-case"
files = ["compose.yaml"]
project_name = "pakro-bootstrap-smoke"
state = "running"
pull = "missing"
wait = true
wait_timeout = 60
depends_on = ["service:docker"]
TOML

cat >"$case_dir/compose.yaml" <<'YAML'
services:
  smoke:
    image: busybox:1.36.1
    command: ["sh", "-c", "while true; do sleep 3600; done"]
YAML

docker build --tag "$image" "$repo_root/tests/support/fixtures/bootstrap-linux-host"
docker run --detach \
	--name "$container" \
	--privileged \
	--cgroupns=host \
	--tmpfs /run \
	--tmpfs /run/lock \
	--volume /sys/fs/cgroup:/sys/fs/cgroup:rw \
	"$image" >/dev/null
docker cp "$pakro_bin" "$container:/opt/pakro"
docker exec "$container" chmod 0755 /opt/pakro

for _ in {1..60}; do
	system_state=$(docker exec "$container" systemctl is-system-running 2>/dev/null || true)
	if [[ $system_state == running || $system_state == degraded ]]; then
		break
	fi
	sleep 1
done
system_state=$(docker exec "$container" systemctl is-system-running 2>/dev/null || true)
if [[ $system_state != running && $system_state != degraded ]]; then
	echo "container systemd failed to start: $system_state" >&2
	exit 1
fi

docker exec --interactive "$container" bash -c \
	'install -d -m 0700 /root/.ssh && install -m 0600 /dev/stdin /root/.ssh/authorized_keys' \
	<"$case_dir/id_ed25519.pub"

host=$(docker inspect "$container" --format '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}')
host_key=$(docker exec "$container" cat /etc/ssh/ssh_host_ed25519_key.pub)
printf '%s %s\n' "$host" "$host_key" >"$case_dir/known_hosts"
ssh_args=(
	-i "$case_dir/id_ed25519"
	-o BatchMode=yes
	-o StrictHostKeyChecking=yes
	-o "UserKnownHostsFile=$case_dir/known_hosts"
	-o SendEnv=-\*
	-o LogLevel=ERROR
	"root@$host"
)

for _ in {1..30}; do
	if ssh "${ssh_args[@]}" true 2>/dev/null; then
		break
	fi
	sleep 1
done
ssh "${ssh_args[@]}" true

remote_bootstrap() {
	"$pakro_bin" bootstrap remote \
		--host "root@$host" \
		--identity-file "$case_dir/id_ed25519" \
		--source "$case_dir" \
		--remote-pakro /opt/pakro \
		--ssh-option StrictHostKeyChecking=yes \
		--ssh-option "UserKnownHostsFile=$case_dir/known_hosts" \
		--ssh-option 'SendEnv=-*' \
		--ssh-option LogLevel=ERROR \
		--only accounts,files,services,firewall,compose \
		--yes
}

remote_bootstrap
remote_bootstrap

ssh "${ssh_args[@]}" \
	'set -eu
   getent passwd pakro-case >/dev/null
   test "$(stat -c %U:%G:%a /opt/pakro-case)" = pakro-case:pakro-case:750
   test "$(stat -c %U:%G:%a /etc/pakro-case.conf)" = root:root:640
   systemctl is-active --quiet pakro-case.service
   systemctl is-enabled --quiet pakro-case.service
   systemctl is-active --quiet docker.service
   nft list table inet pakro_bootstrap | grep -q pakro-bootstrap
   docker compose --project-directory /opt/pakro-case --file /opt/pakro-case/compose.yaml --project-name pakro-bootstrap-smoke ps --status running --quiet | grep -q .
   if systemctl --user show-environment >/dev/null 2>&1; then
     test -f "$HOME/.config/systemd/user/dev.pakro.pakro-case-user.service"
     systemctl --user is-active --quiet dev.pakro.pakro-case-user.service
   else
     echo "no systemd user manager for root on this host; user-service leg reports only"
     ! test -e "$HOME/.config/systemd/user/dev.pakro.pakro-case-user.service"
   fi'

ssh "${ssh_args[@]}" \
	'set -eu
   printf "drift\n" >/etc/pakro-case.conf
   systemctl stop pakro-case.service
   nft add rule inet pakro_bootstrap input tcp dport 9 accept comment '"'"'manual-drift'"'"''

remote_bootstrap

ssh "${ssh_args[@]}" \
	'set -eu
   test "$(cat /etc/pakro-case.conf)" = generation=1
   systemctl is-active --quiet pakro-case.service
   ! nft list table inet pakro_bootstrap | grep -q manual-drift
   docker compose --project-directory /opt/pakro-case --file /opt/pakro-case/compose.yaml --project-name pakro-bootstrap-smoke ps --status running --quiet | grep -q .'
