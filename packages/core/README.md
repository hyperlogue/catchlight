# @catchlight/core

Web-platform glue for Catchlight editor sessions, backends, storage, and viewports.
The package includes a release-built WASM runtime at `@catchlight/core/wasm`.
Importing the package root does not load or initialize it.
`@catchlight/core/fakes` provides test doubles.

```sh
npm install @catchlight/core
```

Initialize the runtime explicitly in your browser application:

```ts
import { Editor, InTabBackend, MemoryStorage } from '@catchlight/core';
import init, * as wasm from '@catchlight/core/wasm';

await init();
const editor = await Editor.create(
  wasm,
  new InTabBackend(new wasm.CatchlightEditor(), new MemoryStorage()),
);
```

The default initializer loads the adjacent `catchlight_bg.wasm` asset. Bundlers
such as Vite emit that asset with the JavaScript. Serve it as `application/wasm`
for streaming compilation. For custom hosting, pass
`await init({ module_or_path: new URL('/assets/catchlight.wasm', location.href) })`.
The binary is exported as `@catchlight/core/wasm/catchlight_bg.wasm` for asset
copying or bundler URL imports. Keep the loader and binary from the same release.

You can still pass a custom implementation of `WasmModule` to `Editor.create`.
React hosts can pass the resulting editor to `CatchlightEditor` from
`@catchlight/editor`, importing `@catchlight/editor/theme.css` for its theme.

[Project and demo](https://github.com/hyperlogue/catchlight#readme).
Licensed under MIT OR Apache-2.0; license texts and third-party notices are
included in the published package.
