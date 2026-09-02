{
  pkgs,
  rustToolchain,
  # The browser the web editor's smoke test drives. Off by default: chromium
  # is by far the largest closure this shell can pull, and only one CI job and
  # one local command ever run the test. `devShells.e2e` in `flake.nix` is this
  # same shell with it on, and it inherits mesa below — which is where the
  # Vulkan ICD headless Chromium needs comes from.
  withBrowser ? false,
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
    name =
      if withBrowser
      then "catchlight-e2e"
      else "catchlight-dev";

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

        # The web editor. `bun` is the package manager and test runner; `nodejs`
        # is here because Vite's plugin ecosystem still shells out to it.
        # `wasm-bindgen-cli` must match the `wasm-bindgen` crate version the
        # workspace pins — a mismatch fails loudly with both versions named.
        bun
        nodejs
        wasm-bindgen-cli

        uv
        python3
      ])
      ++ lib.optionals stdenv.hostPlatform.isLinux [
        # `vulkaninfo --summary` is how you check which ICD the loader actually
        # picked when a headless render fails to find an adapter.
        pkgs.vulkan-tools

        # Mesa, for its CPU Vulkan ICD (lavapipe). Its share/ lands on
        # XDG_DATA_DIRS, so the loader finds lavapipe beside any real driver --
        # no VK_ICD_FILENAMES needed. The browser test needs it too: headless
        # Chromium reaches a WebGPU device through this same ICD.
        pkgs.mesa
      ]
      ++ lib.optionals (withBrowser && stdenv.hostPlatform.isLinux) [
        # The browser `bun run --filter catchlight-site e2e` drives. Playwright's
        # own download is a prebuilt binary that does not run here, so the test
        # takes the executable as an argument and this is what it finds on PATH.
        pkgs.chromium
      ];

    env = lib.optionalAttrs stdenv.hostPlatform.isLinux {
      LD_LIBRARY_PATH = lib.makeLibraryPath runtimeLibs;
    };
  }
