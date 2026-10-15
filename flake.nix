{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    {
      overlay = final: prev: {
        pakro = prev.callPackage ./packaging/nix/default.nix { };
      };
    } // flake-utils.lib.eachDefaultSystem(system:
      let
        pkgs = import nixpkgs { inherit system; };
        pakro = pkgs.callPackage ./packaging/nix/default.nix { };
      in
        {
          packages = {
            inherit pakro;
            default = pakro;
          };

          devShells.default = pkgs.mkShell {
            name = "pakro-develop";

            inputsFrom = [ pakro ];

            nativeBuildInputs = with pkgs; [
              clippy
              rustfmt
              shellcheck
              shfmt
              cargo-insta
            ];
          };
        }
    );
}
