{
  description = "swole: Rust benchmarks counted under stock Valgrind and timed in pairs";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    # The quest CLI, which also serves the quest guide and skills the stubs in
    # .claude/skills call. Bump the rev to upgrade them.
    quest = {
      url = "github:kixel-ai/quest";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.flake-utils.follows = "flake-utils";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      rust-overlay,
      quest,
      ...
    }:
    flake-utils.lib.eachSystem
      [
        "x86_64-linux"
        "aarch64-linux"
        # No x86_64-darwin: nixpkgs no longer evaluates it.
        "aarch64-darwin"
      ]
      (
        system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ (import rust-overlay) ];
          };
          inherit (pkgs) lib;
          linux = pkgs.stdenv.hostPlatform.isLinux;
          toolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
          quest-cli = quest.packages.${system}.default;
          # The Valgrind swole counts under. Another can count differently, so a nixpkgs bump
          # that moves it fails here, and moves only by editing the version, a visible change;
          # its version also starts new series through each count's fingerprint.
          valgrind =
            lib.throwIfNot (pkgs.valgrind.version == "3.27.1")
              "nixpkgs moved Valgrind to ${pkgs.valgrind.version}: bump the version flake.nix pins for swole"
              pkgs.valgrind;
          # The runner, both of its binaries, with its Valgrind on Linux, the one place it
          # counts; it times anywhere.
          cargo-swole =
            (pkgs.makeRustPlatform {
              cargo = toolchain;
              rustc = toolchain;
            }).buildRustPackage
              {
                pname = "cargo-swole";
                version = "0.0.0";
                src = lib.fileset.toSource {
                  root = ./.;
                  fileset = lib.fileset.unions [
                    ./Cargo.toml
                    ./Cargo.lock
                    ./README.md
                    ./crates
                  ];
                };
                cargoLock.lockFile = ./Cargo.lock;
                cargoBuildFlags = [
                  "--package"
                  "cargo-swole"
                ];
                nativeBuildInputs = [ pkgs.makeWrapper ];
                postInstall = lib.optionalString linux ''
                  for bin in swole cargo-swole; do
                    wrapProgram $out/bin/$bin --prefix PATH : ${valgrind}/bin
                  done
                '';
                # The tests run Valgrind: `just test` runs them.
                doCheck = false;
                meta.mainProgram = "swole";
              };
        in
        {
          # Every host times; only x86_64 Linux's counts, which the fingerprint reads CPUID for.
          packages = {
            default = cargo-swole;
            inherit cargo-swole;
            quest = quest-cli;
          }
          # For a program bracketing its benchmarks itself, whose hooks build against the
          # header of the Valgrind swole counts under, so a bump moves both.
          // lib.optionalAttrs linux { inherit valgrind; };
          devShells.default = pkgs.mkShell {
            packages =
              with pkgs;
              [
                toolchain
                quest-cli
                just
                jq
                git
                gh
                cargo-shear
                cargo-sort
                actionlint
                taplo
                nixfmt
              ]
              # The tests count under its Valgrind.
              ++ lib.optional linux valgrind;
          };
          formatter = pkgs.nixfmt;
          checks.quest-tree = pkgs.runCommand "quest-tree" { nativeBuildInputs = [ quest-cli ]; } ''
            quest --root ${self} check
            touch "$out"
          '';
        }
      );
}
