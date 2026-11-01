{
  description = "Translate gettext PO files with PLaMo Translate";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-parts.url = "github:hercules-ci/flake-parts";
    treefmt-nix.url = "github:numtide/treefmt-nix";
  };

  outputs =
    inputs@{ flake-parts, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      systems = [
        "x86_64-linux"
        "aarch64-darwin"
      ];

      imports = [ inputs.treefmt-nix.flakeModule ];

      perSystem =
        { pkgs, ... }:
        {
          packages.default = pkgs.rustPlatform.buildRustPackage {
            pname = "po-plamo";
            version = "0.1.0";
            src = pkgs.lib.cleanSource ./.;
            cargoLock = {
              lockFile = ./Cargo.lock;
              outputHashes."plamo-tr-0.1.0" = "sha256-dhqyW7CiN/9NX52pZ7OBrnePc4veSnqG8aFqgrMKaEM=";
            };
            meta = {
              description = "Translate gettext PO files with PLaMo Translate";
              homepage = "https://github.com/conao3/rust-po-plamo";
              license = pkgs.lib.licenses.asl20;
              mainProgram = "po-plamo";
              platforms = pkgs.lib.platforms.unix;
            };
          };

          devShells.default = pkgs.mkShell {
            packages = with pkgs; [
              cargo
              clippy
              gettext
              gnumake
              rust-analyzer
              rustc
              rustfmt
            ];
          };

          treefmt.programs = {
            nixfmt.enable = true;
            rustfmt.enable = true;
          };
        };
    };
}
