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
    crane.url = "github:ipetkov/crane";
  };

  outputs = inputs @ {
    flake-parts,
    fenix,
    crane,
    ...
  }:
    flake-parts.lib.mkFlake {inherit inputs;} {
      systems = ["x86_64-linux" "aarch64-linux" "aarch64-darwin"];

      perSystem = {
        pkgs,
        lib,
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

        # The system side of the build, in one place: the packages link
        # against these and `nix/shell.nix` puts them on the loader path.
        # wgpu, winit and bevy reach them through `dlopen` at run time, so
        # linking alone is not enough for a `cargo run` out of the shell.
        nativeBuildInputs = [pkgs.pkg-config];
        runtimeLibs = lib.optionals pkgs.stdenv.hostPlatform.isLinux (with pkgs; [
          vulkan-loader
          libxkbcommon
          wayland
          libx11
          libxcursor
          libxi
          libxrandr
        ]);

        craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;

        # Crane's Cargo filter keeps only Rust and Cargo files; the wgpu crate
        # `include_str!`s its shaders, so those have to survive it too. Nothing
        # a build reads lives under `tests/`, so the Git LFS objects there stay
        # out and a plain clone is enough to `nix build`.
        src = lib.cleanSourceWith {
          src = ./.;
          name = "catchlight-source";
          filter = path: type:
            lib.hasSuffix ".wgsl" path || craneLib.filterCargoSources path type;
        };

        commonArgs = {
          inherit src nativeBuildInputs;
          strictDeps = true;
          buildInputs = runtimeLibs;
          # The Rust test suite is local-only: it wants the LFS fixtures and a
          # GPU adapter. These derivations build binaries and nothing else.
          doCheck = false;
        };

        binaries = ["catchlight-editor-server" "catchlight-cli" "catchlight-editor-cli"];

        # One dependency build behind all three binaries, so nothing compiles
        # the graph three times.
        cargoArtifacts = craneLib.buildDepsOnly (commonArgs
          // {
            pname = "catchlight-binaries";
            version = "0.1.0";
            cargoExtraArgs = lib.concatMapStrings (p: " -p ${p}") binaries;
          });

        binary = name:
          craneLib.buildPackage (commonArgs
            // {
              inherit cargoArtifacts;
              pname = name;
              version = "0.1.0";
              cargoExtraArgs = "-p ${name}";
            });
      in {
        packages = lib.genAttrs binaries binary;

        devShells.default = import ./nix/shell.nix {
          inherit pkgs rustToolchain nativeBuildInputs runtimeLibs;
        };

        formatter = pkgs.alejandra;
      };
    };
}
