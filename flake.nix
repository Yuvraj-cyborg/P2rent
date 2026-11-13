{
  description = "p2rent - swarm distribution of large artifacts over QUIC";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };
        manifest = (pkgs.lib.importTOML ./Cargo.toml).package;

        p2rent = pkgs.rustPlatform.buildRustPackage {
          pname = manifest.name;
          version = manifest.version;

          # Only the files that affect the build, so editing docs does not
          # trigger a rebuild.
          src = pkgs.lib.fileset.toSource {
            root = ./.;
            fileset = pkgs.lib.fileset.unions [
              ./Cargo.toml
              ./Cargo.lock
              ./src
              ./tests
              ./benches
            ];
          };
          cargoLock.lockFile = ./Cargo.lock;

          meta = {
            description = manifest.description;
            homepage = manifest.repository;
            license = pkgs.lib.licenses.mit;
            mainProgram = "p2rent";
          };
        };
      in
      {
        packages = {
          inherit p2rent;
          default = p2rent;
        };

        apps.default = flake-utils.lib.mkApp { drv = p2rent; };

        # `nix flake check` builds the package, which runs the test suite.
        checks = { inherit p2rent; };

        devShells.default = pkgs.mkShell {
          inputsFrom = [ p2rent ];
          packages = with pkgs; [
            clippy
            rustfmt
            rust-analyzer
            just
          ];
        };
      });
}
