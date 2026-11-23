# catchlight-editor-cli

Drives a running Catchlight editor session over its Unix socket or HTTP
(`--server URL --token T`). The same commands ship inside `catchlight-editor`;
`catchlight-cli` is the separate tool for model files and offline rendering.

```sh
catchlight-editor-cli session new --name Candidate
catchlight-editor-cli node add --parent root --kind part --name Panel
catchlight-editor-cli history
catchlight-editor-cli help
```

`request` sends any protocol command as JSON, so every server command is
reachable without a dedicated subcommand.

Licensed under MIT OR Apache-2.0.
