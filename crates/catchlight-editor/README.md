# catchlight-editor

The Catchlight editor as one program. It serves the web editor on
`127.0.0.1`, opens it in your browser, and lets local tools and AI agents
drive the same session from the command line while you watch.

```sh
cargo install catchlight-editor
catchlight-editor character.clm        # start the editor and open a model
```

From another terminal, or from an agent:

```sh
catchlight-editor session list
catchlight-editor node tree
catchlight-editor node set face --opacity 0.8
catchlight-editor help                 # every command
```

The editor records its address and a per-launch token in a file only you can
read (`$XDG_RUNTIME_DIR`, `~/.cache` or `%LOCALAPPDATA%` under
`catchlight-editor/connection.json`), which is how commands find it. Running
`catchlight-editor` again opens the editor that is already running.

Server-side previews need a GPU adapter; on Linux without one, install Mesa's
Vulkan drivers for its software renderer. The browser tab renders on its own.

[Project and demos](https://github.com/hyperlogue/catchlight#readme)

Licensed under MIT OR Apache-2.0. See [LICENSE-MIT](LICENSE-MIT),
[LICENSE-APACHE](LICENSE-APACHE), and [third-party notices](THIRD-PARTY-NOTICES.md).
