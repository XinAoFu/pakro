#!/usr/bin/env bash
set -euo pipefail

# Adapt only the test's copy of pakro to the container's native dynamic loader.
mkdir -p /opt /work /test-home
cp /input/pakro /opt/pakro
chmod u+w /opt/pakro
linker=$(nix eval --impure --raw --expr '(import <nixpkgs> {}).stdenv.cc.bintools.dynamicLinker')
patchelf --set-interpreter "$linker" --set-rpath /test-tools/lib /opt/pakro
export PATH="/opt:$PATH"
export HOME=/test-home
export PAKRO_CONFIG_DIR=/test-home/.config/pakro
export PAKRO_DATA_DIR=/test-home/.local/share/pakro
export PAKRO_STATE_DIR=/test-home/.local/state/pakro
export PAKRO_CACHE_DIR=/test-home/.cache/pakro
export PAKRO_YES=1 PAKRO_TRUSTED_CONFIG_PATHS=/work
export PAKRO_USE_VERSIONS_HOST_TRACK=0
cd /work
pakro --version
nix --version

# Config writing, status, and dry-run cannot initialize a profile or evaluate
# the requested source, which intentionally does not exist yet.
pakro bootstrap packages use --no-install 'nix:git+file:///work/flake#hello'
pakro bootstrap packages status --json | jq -e '.nix.packages[0].state == "missing"'
pakro bootstrap packages apply --manager nix --dry-run
test ! -e "$HOME/.nix-profile"
test ! -L "$HOME/.nix-profile"
XDG_STATE_HOME=/work/xdg-state NIX_CONFIG="$NIX_CONFIG
use-xdg-base-directories = true" pakro bootstrap packages status --json | jq -e '.nix.packages[0].state == "missing"'
test ! -e /work/xdg-state/nix/profile
test ! -L /work/xdg-state/nix/profile

mkdir flake
nixpkgs=$(nix eval --impure --raw --expr 'toString <nixpkgs>')
cat >flake/flake.nix <<NIX
{
  inputs.nixpkgs.url = "path:$nixpkgs";
  outputs = { self, nixpkgs }: let
    pkgs = import nixpkgs { system = "x86_64-linux"; };
    version = "v1";
  in {
    packages.x86_64-linux = {
      hello = pkgs.writeShellScriptBin "nix-bootstrap-hello" "echo \${version}";
      unrelated = pkgs.writeShellScriptBin "nix-bootstrap-unrelated" "echo \${version}";
      pinned = pkgs.writeShellScriptBin "nix-bootstrap-pinned" "echo \${version}";
      registryHello = pkgs.writeShellScriptBin "nix-bootstrap-registry" "echo \${version}";
    };
  };
}
NIX
nix flake lock path:/work/flake
git -C flake init -q
git -C flake config user.name 'Nix bootstrap test'
git -C flake config user.email 'test@example.invalid'
git -C flake add flake.nix flake.lock
git -C flake commit -qm 'test: first revision'
first=$(git -C flake rev-parse HEAD)
nix registry add nixpkgs git+file:///work/flake

# Start with an unrelated native profile package, then apply the declaration.
nix profile install 'git+file:///work/flake#unrelated'
pakro bootstrap --only packages
test "$("$HOME/.nix-profile/bin/nix-bootstrap-hello")" = v1
test "$("$HOME/.nix-profile/bin/nix-bootstrap-unrelated")" = v1
before=$(readlink -f "$HOME/.nix-profile")
pakro bootstrap packages apply --manager nix
test "$(readlink -f "$HOME/.nix-profile")" = "$before"
pakro bootstrap packages status --json | jq -e '.nix.packages[0].state == "installed" and (.nix.packages[0].installed_version | startswith("/nix/store/"))'
pakro bootstrap packages use nix:registryHello
test "$("$HOME/.nix-profile/bin/nix-bootstrap-registry")" = v1
before=$(readlink -f "$HOME/.nix-profile")
pakro bootstrap packages apply --manager nix
test "$(readlink -f "$HOME/.nix-profile")" = "$before"

# Source identity matters even when another profile entry has the same attribute.
pakro bootstrap packages apply "nix:git+file:///work/flake?rev=$first#pinned"
pakro bootstrap packages use --no-install "nix:git+file:///work/flake?rev=$first#hello"
pakro bootstrap packages status --json | jq -e '[.nix.packages[] | select(.state == "missing")] | length == 1'
pakro bootstrap packages use --no-install "nix:git+file:///work/flake?rev=$first#pinned"
cat >pakro.toml <<TOML
[bootstrap.packages]
"nix:git+file:///work/flake#hello" = "latest"
"nix:git+file:///work/flake?rev=$first#pinned" = "latest"
TOML

sed -i 's/version = "v1"/version = "v2"/' flake/flake.nix
git -C flake add flake.nix
git -C flake commit -qm 'test: second revision'
pakro bootstrap packages upgrade --manager nix
test "$("$HOME/.nix-profile/bin/nix-bootstrap-hello")" = v2
test "$("$HOME/.nix-profile/bin/nix-bootstrap-unrelated")" = v1
test "$("$HOME/.nix-profile/bin/nix-bootstrap-pinned")" = v1

# Status remains usable with the package source absent and no network fetches.
mv flake hidden-flake
pakro bootstrap packages status --json | jq -e 'all(.nix.packages[]; .state == "installed")'
mv hidden-flake flake

# A failed installation preserves the existing generation.
before=$(readlink -f "$HOME/.nix-profile")
if pakro bootstrap packages apply 'nix:git+file:///work/flake#doesNotExist'; then
	echo 'expected missing attribute to fail' >&2
	exit 1
fi
test "$(readlink -f "$HOME/.nix-profile")" = "$before"

# Export is consumed by the real NixOS module evaluator, with the image's
# pinned package set. Build the selected packages, not a bootable system.
cat >pakro.toml <<'TOML'
[bootstrap.packages]
"nix:hello" = "latest"
"nix:jq" = "latest"
TOML
pakro bootstrap packages export --format nix >packages.nix
cat >system.nix <<NIX
let
  system = import ($nixpkgs + "/nixos") {
    system = "x86_64-linux";
    configuration = { imports = [ ./packages.nix ]; system.stateVersion = "26.05"; };
  };
  pkgs = system.pkgs;
  selected = builtins.filter (p: builtins.elem (p.pname or "") [ "hello" "jq" ]) system.config.environment.systemPackages;
in {
  names = map (p: p.pname) selected;
  bundle = pkgs.buildEnv { name = "pakro-export-test"; paths = selected; };
}
NIX
nix-instantiate --eval --strict --json system.nix -A names | jq -e 'sort == ["hello", "jq"]'
bundle=$(nix-build --no-out-link system.nix -A bundle)
"$bundle/bin/hello"
"$bundle/bin/jq" --version
sed -i '/nix:hello/d' pakro.toml
pakro bootstrap packages export --format nix >packages.nix
nix-instantiate --eval --strict --json system.nix -A names | jq -e '. == ["jq"]'
printf '[bootstrap.packages]\n' >pakro.toml
pakro bootstrap packages export --format nix >packages.nix
nix-instantiate --eval --strict --json system.nix -A names | jq -e '. == []'
pakro bootstrap packages use --no-install nix:thisAttributeDoesNotExist
pakro bootstrap packages export --format nix >packages.nix
if nix-instantiate --eval --strict --json system.nix -A names; then
	echo 'expected missing exported attribute to fail' >&2
	exit 1
fi

echo 'Nix bootstrap Docker checks passed'
