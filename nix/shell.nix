{
  pkgs,
  rustToolchain,
}: let
  inherit (pkgs) stdenv lib;

  mkShell =
    if stdenv.hostPlatform.isLinux
    then
      pkgs.mkShell.override {
        stdenv = pkgs.stdenvAdapters.useMoldLinker pkgs.clangStdenv;
      }
    else pkgs.mkShell;

  # wgpu, winit and bevy reach all of these through `dlopen` at run time, so
  # linking is not enough — they have to be on the loader path of the process
  # `cargo run` / `cargo test` spawns.
  runtimeLibs = lib.optionals stdenv.hostPlatform.isLinux (with pkgs; [
    vulkan-loader
    libxkbcommon
    wayland
    libx11
    libxcursor
    libxi
    libxrandr
  ]);
in
  mkShell {
    name = "catchlight-dev";

    nativeBuildInputs = [
      rustToolchain
      pkgs.pkg-config
    ];

    packages =
      (with pkgs; [
        git-lfs

        rust-analyzer
        cargo-deny
        cargo-edit
        cargo-watch

        uv
        python3
      ])
      ++ lib.optionals stdenv.hostPlatform.isLinux [
        # `vulkaninfo --summary` is how you check which ICD the loader actually
        # picked when a headless render fails to find an adapter.
        pkgs.vulkan-tools

        # Mesa, for its CPU Vulkan ICD (lavapipe). Its share/ lands on
        # XDG_DATA_DIRS, so the loader finds lavapipe beside any real driver --
        # no VK_ICD_FILENAMES needed.
        pkgs.mesa
      ];

    env = lib.optionalAttrs stdenv.hostPlatform.isLinux {
      LD_LIBRARY_PATH = lib.makeLibraryPath runtimeLibs;
    };
  }
