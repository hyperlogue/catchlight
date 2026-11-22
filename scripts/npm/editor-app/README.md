# @catchlight/editor

The Catchlight editor as one program. It serves the web editor on
`127.0.0.1`, opens it in your browser, and lets local tools and AI agents
drive the same session from the command line while you watch.

```sh
npx @catchlight/editor character.clm   # start the editor and open a model
```

From another terminal, or from an agent:

```sh
npx @catchlight/editor session list
npx @catchlight/editor node tree
npx @catchlight/editor help            # every command
```

Install it globally (`npm install -g @catchlight/editor`) to get a
`catchlight-editor` command. npm installs only the build for your platform
(Linux x64/arm64, macOS arm64/x64, Windows x64); the same binaries are on the
[GitHub releases](https://github.com/hyperlogue/catchlight/releases) and on
crates.io as `catchlight-editor`.

The embeddable React editor component that used to be published under this
name is [`@catchlight/editor-ui`](https://www.npmjs.com/package/@catchlight/editor-ui).

Licensed under MIT OR Apache-2.0. See [LICENSE-MIT](LICENSE-MIT),
[LICENSE-APACHE](LICENSE-APACHE), and [third-party notices](THIRD-PARTY-NOTICES.md).
