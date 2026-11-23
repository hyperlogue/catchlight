# catchlight (Python)

A blocking, dependency-free Python client for `catchlight-editor-server`.

```sh
python -m pip install catchlight
```

Linux x86-64 and AArch64 wheels (Python 3.11+, `manylinux_2_28`) bundle the
matching server. Elsewhere, install the server separately and pass `binary=`
to `launch()`, or attach to a running one with `catchlight.connect(url, token)`.

```python
import catchlight

with catchlight.launch() as server:
    client = server.client()
    built = catchlight.Builder.new(client, "Doll")
    face = built.part("Face", "face.png")
    tilt = built.param("Tilt", min=-1.0, max=1.0)
    built.bind(tilt, face, "ty", [(-1.0, -10.0), (0.0, 0.0), (1.0, 10.0)])
    built.save_to("doll.clm")
```

Every protocol command is available as a generated dataclass through
`client.send`. A larger workflow is in
[`examples/rig_workflow.py`](https://github.com/hyperlogue/catchlight/blob/main/python/examples/rig_workflow.py).

## Development

```sh
cargo build -p catchlight-editor-server
cd python && uv run pytest -q
```

Licensed under MIT OR Apache-2.0.
