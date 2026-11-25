"""The Catchlight editor protocol and a blocking client that speaks it.

The protocol is star-imported from the generated `protocol_gen.py` so there is
no second hand-kept roster to drift.

    with catchlight.launch() as server:
        built = catchlight.build_from_layers(server.client(), "layers.json")
        built.save_to("doll.clm")
"""

from .builder import Builder, BuilderError, build_from_layers
from .client import Client, Maintained, ProtocolError
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
    "Maintained",
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
