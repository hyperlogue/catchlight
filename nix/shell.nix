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

        # Mesa, for its CPU Vulkan ICD (lavapipe)
        pkgs.mesa
      ];

    env = lib.optionalAttrs stdenv.hostPlatform.isLinux {
      LD_LIBRARY_PATH = lib.makeLibraryPath runtimeLibs;

      # Path to mesa's CPU Vulkan ICD (lavapipe). Export it as
      # `VK_ICD_FILENAMES` to run the headless GPU tests on a box with no real
      # GPU — that is what CI does. It is deliberately *not* set as
      # `VK_ICD_FILENAMES` here: doing so would force lavapipe over a real
      # driver for everyone working in the shell.
      CATCHLIGHT_LAVAPIPE_ICD = "${pkgs.mesa}/share/vulkan/icd.d/lvp_icd.x86_64.json";
    };
  }
