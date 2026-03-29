{
  description = "p2rent - Blazing-fast P2P file sharing over QUIC";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };
      in
      {
        packages = {
          p2rent = pkgs.rustPlatform.buildRustPackage {
            pname = "p2rent";
            version = "0.1.0";
            src = ./.;
            cargoLock.lockFile = ./Cargo.lock;

            meta = with pkgs.lib; {
              description = "Blazing-fast P2P file sharing over QUIC";
              license = licenses.mit;
              mainProgram = "p2rent";
            };
          };
          default = self.packages.${system}.p2rent;
        };

        devShells.default = pkgs.mkShell {
          buildInputs = with pkgs; [
            rustc
            cargo
            clippy
            rustfmt
            rust-analyzer
          ];
        };
      });
}
