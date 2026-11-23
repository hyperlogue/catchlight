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
        releaseVersion = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;
        # The editor must build for the web, so `cargo check` needs this target.
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

        # wgpu, winit and bevy `dlopen` these at run time, so linking alone is
        # not enough for a `cargo run` out of the shell.
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

        # Keep shaders, crate Markdown embedded by rustdoc, and the render
        # schema's JSON example. Git LFS model fixtures stay out, so a plain
        # clone is enough to build the binaries.
        src = lib.cleanSourceWith {
          src = ./.;
          name = "catchlight-source";
          filter = path: type:
            lib.hasSuffix ".wgsl" path
            || lib.hasSuffix "/LICENSE-MIT" path
            || lib.hasSuffix "/LICENSE-APACHE" path
            || path == toString ./THIRD-PARTY-NOTICES.md
            || (lib.hasSuffix ".md" path && lib.hasPrefix "${toString ./crates}/" path)
            || path == toString ./crates/catchlight-cli/tests/fixtures/render-spec.json
            || path == toString ./crates/catchlight-importer/src/livliv/model-v4.json
            || path == toString ./crates/catchlight-importer/src/livliv/keyforms-v1.json
            || craneLib.filterCargoSources path type;
        };

        commonArgs = {
          inherit src nativeBuildInputs;
          strictDeps = true;
          buildInputs = runtimeLibs;
          # Tests need LFS fixtures and a GPU, so they run locally only.
          doCheck = false;
        };

        binaries = ["catchlight-editor-server" "catchlight-cli" "catchlight-editor-cli"];

        cargoArtifacts = craneLib.buildDepsOnly (commonArgs
          // {
            pname = "catchlight-binaries";
            version = releaseVersion;
            cargoExtraArgs = lib.concatMapStrings (p: " -p ${p}") binaries;
          });

        binary = name:
          craneLib.buildPackage (commonArgs
            // {
              inherit cargoArtifacts;
              pname = name;
              version = releaseVersion;
              cargoExtraArgs = "-p ${name}";
            });
        # The importer needs neither the editor's dependency build nor GPU libs.
        importerArgs = {
          inherit src;
          pname = "catchlight-importer";
          version = releaseVersion;
          cargoExtraArgs = "-p catchlight-importer --features cli";
          strictDeps = true;
          doCheck = false;
        };
      in {
        packages =
          lib.genAttrs binaries binary
          // {
            catchlight-importer = craneLib.buildPackage (importerArgs
              // {
                cargoArtifacts = craneLib.buildDepsOnly importerArgs;
              });
          };

        devShells.default = import ./nix/shell.nix {
          inherit pkgs rustToolchain nativeBuildInputs runtimeLibs;
        };

        formatter = pkgs.alejandra;
      };
    };
}
