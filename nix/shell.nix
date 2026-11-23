{
  pkgs,
  rustToolchain,
  nativeBuildInputs,
  runtimeLibs,
}: let
  inherit (pkgs) stdenv lib;

  mkShell =
    if stdenv.hostPlatform.isLinux
    then
      pkgs.mkShell.override {
        stdenv = pkgs.stdenvAdapters.useMoldLinker pkgs.clangStdenv;
      }
    else pkgs.mkShell;
in
  mkShell {
    name = "catchlight-dev";

    nativeBuildInputs = [rustToolchain] ++ nativeBuildInputs;

    packages =
      (with pkgs; [
        git-lfs

        rust-analyzer
        cargo-deny
        cargo-edit
        cargo-watch

        # Vite's plugins still shell out to `nodejs`. `wasm-bindgen-cli` must
        # match the workspace's `wasm-bindgen` crate version.
        bun
        nodejs
        wasm-bindgen-cli

        uv
        (python3.withPackages (ps: [ps.numpy]))

        ffmpeg-headless
      ])
      ++ lib.optionals stdenv.hostPlatform.isLinux [
        # `vulkaninfo --summary` is how you check which ICD the loader actually
        # picked when a headless render fails to find an adapter.
        pkgs.vulkan-tools

        # Mesa's lavapipe CPU Vulkan ICD, found via XDG_DATA_DIRS; headless
        # Chromium reaches WebGPU through it too.
        pkgs.mesa

        # Playwright's downloaded browser does not run on NixOS, so the e2e
        # test takes this executable from PATH.
        pkgs.chromium
        # WebGPU presentation tests need an X11 compositor surface.
        pkgs.xvfb-run
      ];

    env = lib.optionalAttrs stdenv.hostPlatform.isLinux {
      LD_LIBRARY_PATH = lib.makeLibraryPath runtimeLibs;
    };
  }
