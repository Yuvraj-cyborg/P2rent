{
  description = "mux - swarm distribution of large artifacts over QUIC";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };
        workspace = (pkgs.lib.importTOML ./Cargo.toml).workspace.package;
        manifest = (pkgs.lib.importTOML ./crates/cli/Cargo.toml).package;

        mux = pkgs.rustPlatform.buildRustPackage {
          pname = "mux";
          version = workspace.version;

          # Only the files that affect the build, so editing docs does not
          # trigger a rebuild.
          src = pkgs.lib.fileset.toSource {
            root = ./.;
            fileset = pkgs.lib.fileset.unions [
              ./Cargo.toml
              ./Cargo.lock
              ./crates
              ./xtask
            ];
          };
          cargoLock.lockFile = ./Cargo.lock;

          meta = {
            description = manifest.description;
            homepage = workspace.repository;
            license = pkgs.lib.licenses.mit;
            mainProgram = "mux";
          };
        };
      in
      {
        packages = {
          inherit mux;
          default = mux;
        };

        apps.default = flake-utils.lib.mkApp { drv = mux; };

        # `nix flake check` builds the package, which runs the test suite.
        checks = { inherit mux; };

        devShells.default = pkgs.mkShell {
          inputsFrom = [ mux ];
          packages = with pkgs; [
            clippy
            rustfmt
            rust-analyzer
            just
          ];
        };
      });
}
