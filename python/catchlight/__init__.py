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
"""

from .client import Client, ProtocolError
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
    "ByteTransport",
    "Client",
    "HttpTransport",
    "LaunchedServer",
    "ProtocolError",
    "ServerError",
    "Transport",
    "TransportError",
    "UnixSocketTransport",
    "connect",
    "launch",
]
