"""The catchlight editor protocol, and a blocking client that speaks it.

The protocol half comes from `protocol_gen.py`, which `cargo xtask generate
python` writes from the Rust wire types. Re-exported with a star import rather
than a hand-written list on purpose: a list would be a second roster of the
protocol to keep in step, which is the drift the generator exists to stop.

The client half is written by hand and named here: `launch` starts an editor
and owns it, `connect` attaches to one somebody else runs, and both hand back a
`Client` whose `send` takes any command in the protocol.

    with catchlight.launch() as server:
        client = server.client()
        session = client.new()
        part = client.add_part(session, name="Body")
        client.add_texture(session, part, "body.png")
        client.send(catchlight.MeshAuto(session=session, node=part))
        client.save_to(session, "body.clm")

Over that sits the authoring half: `Builder` says a group, a part, a param and
a binding in one call each, and `read_layers` reads the placement file the
reference-image pipeline hands over, so a whole model is one `build_from_layers`.

    with catchlight.launch() as server:
        built = catchlight.build_from_layers(server.client(), "layers.json")
        built.save_to("doll.clm")
"""

from .builder import Builder, BuilderError, build_from_layers
from .client import Client, ProtocolError
from .layers import (
    LAYERS_VERSION,
    Layer,
    LayersError,
    Placement,
    read_layers,
    write_layers,
)
from .protocol_gen import *  # noqa: F403
from .protocol_gen import __all__ as _protocol_all
from .server import LaunchedServer, ServerError, connect, launch
from .transport import (
    ByteTransport,
    HttpTransport,
    Transport,
    TransportError,
    UnixSocketTransport,
)

__all__ = [
    *_protocol_all,
    "LAYERS_VERSION",
    "Builder",
    "BuilderError",
    "ByteTransport",
    "Client",
    "HttpTransport",
    "LaunchedServer",
    "Layer",
    "LayersError",
    "Placement",
    "ProtocolError",
    "ServerError",
    "Transport",
    "TransportError",
    "UnixSocketTransport",
    "build_from_layers",
    "connect",
    "launch",
    "read_layers",
    "write_layers",
]
