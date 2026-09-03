"""One blocking client over one transport: send a command, get its body.

Every command in the protocol goes through [`Client.send`]. What differs
between commands is what sending one *means*, and the protocol writes that down
as `CommandKind` — so this client routes on the kind it is given rather than
offering a method per kind that a caller could pick wrong.

Invariants this module enforces:

- **One send, routed by kind.** A `document` command moves the session's
  revision and the reply says what it moved to; a `presence` or `scratch`
  command publishes view state and moves nothing; the two query kinds read.
  `send` returns the reply's body for all of them, and records a revision for
  the first.

- **A refused command raises, a broken connection raises differently.** An
  editor that answered `err` is working — the command was wrong — so that is a
  [`ProtocolError`] carrying the `ErrorCode` a caller branches on. A connection
  that failed is a `TransportError` from the transport.

- **The revision is what this client last saw.** It comes from replies, never
  from events: a blocking caller acts on the answer to its own command, and an
  event says only that somebody else's landed.

- **A path is absolute before it is sent.** The wire carries a storage key, and
  what a relative one resolves against is the *server's* store root, not this
  script's directory. Every convenience method here makes the path absolute, so
  the two agree without either having to know the other's working directory.

- **Bytes go where the store is.** Over the socket the editor reads the very
  filesystem this script is on, so a texture is a path and a save is a save.
  Over HTTP it is somewhere else: a texture is staged with `PUT /files/…` and
  then named, and a save is the document fetched back and written here.
"""

from __future__ import annotations

import os
import uuid
from pathlib import Path

from .protocol_gen import (
    Command,
    CommandKind,
    ErrorCode,
    Event,
    NodeAdd,
    NodeKindArg,
    NodeId,
    ReplyErr,
    ReplyOk,
    ResponseBody,
    ResponseBodyNode,
    ResponseBodySaved,
    ResponseBodySession,
    ResponseBodyTexture,
    Save,
    SessionClose,
    SessionId,
    SessionNew,
    SessionOpen,
    TexId,
    TextureAdd,
    parse_event,
    parse_reply,
)
from .transport import ByteTransport, Transport

__all__ = ["Client", "ProtocolError"]


class ProtocolError(RuntimeError):
    """The editor refused a command. `code` is what to branch on."""

    def __init__(self, code: ErrorCode, message: str) -> None:
        super().__init__(f"{code.value}: {message}")
        self.code = code
        self.message = message


class Client:
    """An editing session's worth of commands, over one transport."""

    def __init__(self, transport: Transport) -> None:
        self._transport = transport
        self._revisions: dict[SessionId, int] = {}

    def __enter__(self) -> Client:
        return self

    def __exit__(self, *_exc: object) -> None:
        self.close()

    def close(self) -> None:
        self._transport.close()

    # -- the one send

    def send(self, command: Command) -> ResponseBody:
        """Send any command and return the body of its reply.

        Raises [`ProtocolError`] if the editor refused it.
        """
        reply = parse_reply(self._transport.request(command.to_wire()))
        if isinstance(reply, ReplyErr):
            raise ProtocolError(reply.code, reply.message)
        if not isinstance(reply, ReplyOk):
            raise ProtocolError(
                ErrorCode.BAD_REQUEST,
                f"expected a reply to {type(command).CMD}, got an event",
            )
        self._record(command, reply)
        return reply.body

    def events(self) -> list[Event]:
        """Every event that arrived unasked since the last call.

        Always empty over the socket, which pushes none.
        """
        return [parse_event(message) for message in self._transport.events()]

    def revision(self, session: SessionId) -> int | None:
        """The revision this client's last reply reported for `session`."""
        return self._revisions.get(session)

    def _record(self, command: Command, reply: ReplyOk) -> None:
        """A document command's reply names the revision it produced.

        A `session_close` is the one that names none — its session is gone —
        so it drops the entry instead of moving it.
        """
        if isinstance(command, SessionClose):
            self._revisions.pop(command.session, None)
            return
        if type(command).KIND is not CommandKind.DOCUMENT or reply.rev is None:
            return
        session = getattr(command, "session", None)
        if session is None and isinstance(reply.body, ResponseBodySession):
            # The create/open/import commands name no session; they make one.
            session = reply.body.session
        if session is not None:
            self._revisions[session] = reply.rev

    # -- sessions

    def new(self, name: str | None = None) -> SessionId:
        """An empty session. `name` is a title only; nothing addresses it."""
        return _session(self.send(SessionNew(name=name)))

    def open(self, path: str | os.PathLike[str]) -> SessionId:
        """Open a `.clm` into a session.

        `path` is made absolute, so it names the same file to the editor as it
        does here — as long as the editor is on this filesystem. Over HTTP it
        is a key in the server's own store instead.
        """
        return _session(self.send(SessionOpen(path=_absolute(path))))

    def close_session(self, session: SessionId) -> None:
        """Discard a session and its undo history. Unsaved edits are lost."""
        self.send(SessionClose(session=session))

    # -- output

    def save(self, session: SessionId, key: str | None = None) -> str:
        """Write the document into the editor's store and return the key.

        `key` absent saves back over what the session was opened from, which a
        session created from an upload does not have.
        """
        return _saved(self.send(Save(session=session, path=key)))

    def save_to(self, session: SessionId, path: str | os.PathLike[str]) -> str:
        """Write the document to a local file and return its path.

        Over the socket that is a save, because the editor's store is this
        filesystem. Over HTTP the editor's store is somewhere else, so the
        document is fetched and written here.
        """
        target = _absolute(path)
        transport = self._transport
        if isinstance(transport, ByteTransport):
            Path(target).write_bytes(transport.get_clm(session))
            return target
        return _saved(self.send(Save(session=session, path=target)))

    # -- content

    def add_part(
        self,
        session: SessionId,
        parent: NodeId = "root",
        name: str | None = None,
    ) -> NodeId:
        """Add a part under `parent` and return the Id the editor minted."""
        body = self.send(
            NodeAdd(session=session, parent=parent, kind=NodeKindArg.PART, name=name)
        )
        if not isinstance(body, ResponseBodyNode):
            raise ProtocolError(ErrorCode.BAD_REQUEST, f"node_add answered {body!r}")
        return body.node

    def add_texture(
        self,
        session: SessionId,
        node: NodeId,
        path: str | os.PathLike[str],
    ) -> TexId:
        """Give `node` the image at `path`, and return the texture's Id.

        Over the socket the editor opens the file itself. Over HTTP it cannot,
        so the bytes are staged under a key first and the command names that
        key; the key keeps the file's extension, which is what the editor reads
        the encoding from.
        """
        source = Path(_absolute(path))
        transport = self._transport
        if isinstance(transport, ByteTransport):
            key = f"upload/{uuid.uuid4().hex}{source.suffix}"
            transport.put_file(key, source.read_bytes())
        else:
            key = str(source)
        body = self.send(TextureAdd(session=session, node=node, path=key))
        if not isinstance(body, ResponseBodyTexture):
            raise ProtocolError(ErrorCode.BAD_REQUEST, f"texture_add answered {body!r}")
        return body.texture


def _absolute(path: str | os.PathLike[str]) -> str:
    return os.path.abspath(os.fspath(path))


def _session(body: ResponseBody) -> SessionId:
    if not isinstance(body, ResponseBodySession):
        raise ProtocolError(ErrorCode.BAD_REQUEST, f"expected a session, got {body!r}")
    return body.session


def _saved(body: ResponseBody) -> str:
    if not isinstance(body, ResponseBodySaved):
        raise ProtocolError(ErrorCode.BAD_REQUEST, f"expected a saved path, got {body!r}")
    return body.path
