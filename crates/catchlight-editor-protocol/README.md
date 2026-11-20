# catchlight-editor-protocol

Transport-independent request, reply, and event types for the Catchlight
editor. Messages use JSON and carry the same model identifiers across native
Unix-socket and browser WebSocket clients.

Use this crate to build editor clients. The optional `ts` feature enables
TypeScript declaration generation; ordinary clients do not need it.

```sh
cargo add catchlight-editor-protocol
```

[API documentation](https://docs.rs/catchlight-editor-protocol) ·
[Project and demos](https://github.com/hyperlogue/catchlight#readme)

Licensed under MIT OR Apache-2.0. See [LICENSE-MIT](LICENSE-MIT),
[LICENSE-APACHE](LICENSE-APACHE), and [third-party notices](THIRD-PARTY-NOTICES.md).
