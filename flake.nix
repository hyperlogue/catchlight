{
  inputs = {
    nixpkgs.url = "nixpkgs/nixos-unstable";
    flake-parts = {
      url = "github:hercules-ci/flake-parts";
      inputs.nixpkgs-lib.follows = "nixpkgs";
    };
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = inputs @ {
    flake-parts,
    fenix,
    ...
  }:
    flake-parts.lib.mkFlake {inherit inputs;} {
      systems = ["x86_64-linux" "aarch64-linux" "aarch64-darwin"];

      perSystem = {
        pkgs,
        system,
        ...
      }: let
        # `wasm32-unknown-unknown` is not optional tooling here: the editor is
        # required to run on the web, so a plain `cargo check` of the editor
        # crates needs the target's `rust-std` present in the same toolchain.
        rustToolchain = fenix.packages.${system}.combine [
          (fenix.packages.${system}.stable.withComponents [
            "cargo"
            "clippy"
            "rust-src"
            "rustc"
            "rustfmt"
          ])
          fenix.packages.${system}.targets.wasm32-unknown-unknown.stable.rust-std
        ];
      in {
        devShells = {
          default = import ./nix/shell.nix {
            inherit pkgs rustToolchain;
          };

          # The same shell plus the browser the web editor's smoke test drives.
          # Its own output because chromium's closure is the largest thing any
          # of this pulls, and every CI job but one would download it for
          # nothing: `nix develop .#e2e -c bun run --filter catchlight-site e2e`
          # is the only command that wants it.
          e2e = import ./nix/shell.nix {
            inherit pkgs rustToolchain;
            withBrowser = true;
          };
        };

        formatter = pkgs.alejandra;
      };
    };
}
