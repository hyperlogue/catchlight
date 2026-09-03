"""The catchlight editor protocol.

Everything here comes from `protocol_gen.py`, which `cargo xtask generate
python` writes from the Rust wire types. Re-exported with a star import rather
than a hand-written list on purpose: a list would be a second roster of the
protocol to keep in step, which is the drift the generator exists to stop.
"""

from .protocol_gen import *  # noqa: F403
from .protocol_gen import __all__ as _protocol_all

__all__ = list(_protocol_all)
