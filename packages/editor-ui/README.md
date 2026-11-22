# @catchlight/editor-ui

An embeddable React 19 editor assembled from `@catchlight/react`, with a
default theme. Pass an `Editor` from `@catchlight/core` to `CatchlightEditor`
and import `@catchlight/editor-ui/theme.css`. Install `@catchlight/core`,
`@catchlight/react`, `react`, and `react-dom` as peers.

The host chooses the backend and initializes the WASM runtime bundled in
`@catchlight/core/wasm`. This package does not duplicate that binary or include
a native server or application launcher. See the core package's initialization
example before passing the resulting editor to `CatchlightEditor`.

```sh
npm install @catchlight/editor-ui
```

[Project and demo](https://github.com/hyperlogue/catchlight#readme).
Licensed under MIT OR Apache-2.0; license texts and third-party notices are
included in the published package.
