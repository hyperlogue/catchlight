"""One blocking client over one transport: send a command, get its body.

Every command in the protocol goes through [`Client.send`]. What differs
between commands is what sending one *means*, and the protocol writes that down
as `CommandKind` — so this client routes on the kind it is given rather than
offering a method per kind that a caller could pick wrong.

Invariants this module enforces:

- **One send, routed by kind.** An `edit` command moves the session's
  revision when authored content changes. Presence publishes view state,
  while the query kinds read. `send` records each captured reply revision,
  including reads, without inferring a newer revision from another request.

- **A refused command raises, a broken connection raises differently.** An
  editor that answered `err` is working — the command was wrong — so that is a
  [`ProtocolError`] carrying the `ErrorCode` a caller branches on. A connection
  that failed is a `TransportError` from the transport.

- **The revision is what this client last saw.** It comes from replies and
  from nothing else. Neither door this client speaks hears an event — those go
  to the browser tab's WebSocket — so a blocking caller acts on the answer to
  its own command.

- **A path is absolute before it is sent.** The wire carries a storage key, and
  what a relative one resolves against is the *server's* store root, not this
  script's directory. Every convenience method here makes the path absolute, so
  the two agree without either having to know the other's working directory.

- **Bytes go where the store is.** A *stored* file is still the store's: over
  the socket the editor reads the very filesystem this script is on, so a save
  is a save and `open` names a file it can read; over HTTP the store is
  somewhere else, so a save is the model fetched back and written here.

- **Bytes a command needs travel with it.** An image, a `.clm`, a manifest and
  its textures are read here and handed to `send_with`, which puts them beside
  the command whichever door is underneath. Nothing is staged first, so there
  is no key to name, no upload to clean up, and the same call works over both
  transports.

- **An extension's kind is its Python type.** `extension_set` files `bytes` as
  a byte extension and anything else as JSON, and `extension_get` gives back
  what was written. Catchlight reads neither: a value goes in, the same value
  comes out of the next save, and nothing here interprets it.
"""

from __future__ import annotations

import json
import os
from collections.abc import Iterable, Mapping, Sequence
from pathlib import Path
from typing import Any

from .protocol_gen import (
    Camera,
    Command,
    ErrorCode,
    EditApply,
    EditValidate,
    EditOp,
    EditGoto,
    EditHistoryGet,
    LimitInfo,
    ModelExport,
    Undo,
    Redo,
    Status,
    ExtensionDelete,
    ExtensionGet,
    ExtensionInfo,
    ExtensionKey,
    ExtensionSetBytes,
    ExtensionSetJson,
    Extensions,
    CommandExtensionSet,
    ImportJson,
    ImportTexture,
    NodeAdd,
    NodeKindArg,
    NodeId,
    ParamPose,
    Preview,
    ReplyErr,
    ReplyOk,
    ResponseBody,
    ExtensionValueInfoBytes,
    ExtensionValueInfoJson,
    ResponseBodyExtension,
    ResponseBodyExtensions,
    ResponseBodyNode,
    ResponseBodyPreview,
    ResponseBodySaved,
    ResponseBodySession,
    ResponseBodySessionFork,
    ResponseBodyModelExport,
    ResponseBodyEditResults,
    ResponseBodyEditHistory,
    ResponseBodyTexture,
    Save,
    SessionClose,
    SessionId,
    SessionNew,
    SessionFork,
    SessionSourceClm,
    SessionSourceJson,
    SessionSourceManifest,
    SessionOpen,
    TexId,
    TextureAdd,
    TextureAlpha,
    TextureEncoding,
    parse_reply,
)
from .transport import ByteTransport, Transport

__all__ = ["Client", "ProtocolError"]


class ProtocolError(RuntimeError):
    """The editor refused a command. `code` is what to branch on."""

    def __init__(
        self, code: ErrorCode, message: str, *,
        op_index: int | None = None, limit: LimitInfo | None = None,
    ) -> None:
        super().__init__(f"{code.value}: {message}")
        self.code = code
        self.message = message
        self.op_index = op_index
        self.limit = limit


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
            raise ProtocolError(
                reply.code, reply.message, op_index=reply.op_index, limit=reply.limit
            )
        if not isinstance(reply, ReplyOk):
            raise ProtocolError(
                ErrorCode.BAD_REQUEST,
                f"expected a reply to {type(command).CMD}, got an event",
            )
        self._record(command, reply)
        return reply.body

    def send_with(
        self,
        command: Command,
        attachments: Mapping[str, bytes] | None = None,
    ) -> tuple[ResponseBody, bytes | None]:
        """[`Client.send`] for a command that carries bytes.

        The attachments go beside the command and the payload comes back beside
        the reply, whichever door the transport is; what the framing looks like
        is [`catchlight.transport`]'s business and differs between the two.
        """
        wire, payload = self._transport.request_with(command.to_wire(), attachments)
        reply = parse_reply(wire)
        if isinstance(reply, ReplyErr):
            raise ProtocolError(
                reply.code, reply.message, op_index=reply.op_index, limit=reply.limit
            )
        if not isinstance(reply, ReplyOk):
            raise ProtocolError(
                ErrorCode.BAD_REQUEST,
                f"expected a reply to {type(command).CMD}, got an event",
            )
        self._record(command, reply)
        return reply.body, payload

    def revision(self, session: SessionId) -> int | None:
        """The revision this client's last reply reported for `session`."""
        return self._revisions.get(session)

    def _record(self, command: Command, reply: ReplyOk) -> None:
        """Remember the exact captured revision on every successful model read.

        A fork's outer revision belongs to its new session; its source remains
        at the independently captured revision named in the response body.
        """
        if isinstance(command, SessionClose):
            self._revisions.pop(command.session, None)
            return
        if reply.rev is None:
            return
        if isinstance(reply.body, ResponseBodySessionFork):
            self._revisions[reply.body.session] = reply.rev
            self._revisions[reply.body.source.session] = reply.body.source.rev
            return
        session = getattr(command, "session", None)
        if session is None and isinstance(reply.body, ResponseBodySession):
            session = reply.body.session
        if session is not None:
            self._revisions[session] = reply.rev

    def require_revision(self, session: SessionId, if_rev: int | None = None) -> int:
        """Choose an explicit guard or the last captured revision.

        A session this client has not read is queried once. A stale known
        revision is preserved so the server refuses the write; no command is
        automatically retried against someone else's newer work.
        """
        if if_rev is not None:
            return if_rev
        if session not in self._revisions:
            self.send(Status(session=session))
        revision = self.revision(session)
        if revision is None:
            raise ProtocolError(ErrorCode.BAD_REQUEST, "reply omitted session revision")
        return revision

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
        """Write the model into the editor's store and return the key.

        `key` absent saves back over what the session was opened from, which a
        session created from an upload does not have.
        """
        return _saved(self.send(Save(session=session, path=key)))

    def save_to(self, session: SessionId, path: str | os.PathLike[str]) -> str:
        """Write the model to a local file and return its path.

        Over the socket that is a save, because the editor's store is this
        filesystem. Over HTTP the editor's store is somewhere else, so the
        model is fetched and written here.
        """
        target = _absolute(path)
        transport = self._transport
        if isinstance(transport, ByteTransport):
            Path(target).write_bytes(self.export_model(session))
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

        The bytes are read here and travel with the command, over either door.
        The encoding is decided here too, from the file's suffix, because it is
        a field on the command now rather than something the editor sniffs off
        a key.
        """
        source = Path(_absolute(path))
        body, _ = self.send_with(
            TextureAdd(
                session=session,
                node=node,
                encoding=_encoding_of(source),
            ),
            {"texture": source.read_bytes()},
        )
        if not isinstance(body, ResponseBodyTexture):
            raise ProtocolError(ErrorCode.BAD_REQUEST, f"texture_add answered {body!r}")
        return body.texture

    # -- complete session sources and subtree installation

    def from_file(
        self, path: str | os.PathLike[str], *, name: str | None = None,
    ) -> SessionId:
        """Create a clean revision-zero session from local `.clm` bytes.

        The new session has no save path. `open` instead reads a store-owned
        file and remembers that path for later saves.
        """
        body, _ = self.send_with(
            SessionNew(name=name, source=SessionSourceClm()),
            {"model": Path(_absolute(path)).read_bytes()},
        )
        return _session(body)

    def from_json(
        self,
        structure: Mapping[str, Any] | str,
        textures: Mapping[str, str | os.PathLike[str]]
        | Iterable[tuple[str, str | os.PathLike[str]]] = (),
        *,
        name: str | None = None,
        alpha: TextureAlpha = TextureAlpha.STRAIGHT,
    ) -> SessionId:
        """Create a session from a complete CLM structure and local images.

        Binary extensions need a CLM source, because JSON contains their
        markers without their payloads. Construction failure creates no session.
        """
        declared, attachments = _structure_attachments(structure, textures, alpha)
        body, _ = self.send_with(
            SessionNew(name=name, source=SessionSourceJson(textures=declared)),
            attachments,
        )
        return _session(body)

    def from_manifest(
        self, manifest_path: str | os.PathLike[str], *, name: str | None = None,
    ) -> SessionId:
        """Create a session from a manifest and its images in one request."""
        source = Path(_absolute(manifest_path))
        manifest = source.read_bytes()
        attachments: dict[str, bytes] = {"manifest": manifest}
        for reference in _texture_references(manifest, source):
            attachments[f"texture:{reference}"] = (source.parent / reference).read_bytes()
        body, _ = self.send_with(
            SessionNew(name=name, source=SessionSourceManifest()), attachments
        )
        return _session(body)

    def import_json(
        self,
        session: SessionId,
        structure: Mapping[str, Any] | str,
        textures: Mapping[str, str | os.PathLike[str]]
        | Iterable[tuple[str, str | os.PathLike[str]]] = (),
        *,
        parent: NodeId,
        if_rev: int | None = None,
        alpha: TextureAlpha = TextureAlpha.STRAIGHT,
    ) -> None:
        """Install structure roots beneath `parent` as one guarded edit.

        Existing IDs must not collide. Use `from_json` for a complete new
        session; this operation always installs into an existing model.
        """
        declared, attachments = _structure_attachments(structure, textures, alpha)
        self.send_with(
            ImportJson(
                session=session, parent=parent,
                if_rev=self.require_revision(session, if_rev), textures=declared,
            ),
            attachments,
        )

    def fork(
        self, session: SessionId, *, name: str | None = None,
        if_rev: int | None = None,
    ) -> SessionId:
        """Copy one captured authored state into an independent clean session."""
        body = self.send(SessionFork(
            session=session, if_rev=self.require_revision(session, if_rev), name=name,
        ))
        if not isinstance(body, ResponseBodySessionFork):
            raise ProtocolError(ErrorCode.BAD_REQUEST, f"session_fork answered {body!r}")
        return body.session

    def export_model(self, session: SessionId, *, if_rev: int | None = None) -> bytes:
        """Return a guarded CLM snapshot without changing save state or history."""
        body, payload = self.send_with(ModelExport(
            session=session, if_rev=self.require_revision(session, if_rev),
        ))
        if not isinstance(body, ResponseBodyModelExport) or payload is None:
            raise ProtocolError(ErrorCode.BAD_REQUEST, f"model_export answered {body!r}")
        if len(payload) != body.byte_length:
            raise ProtocolError(ErrorCode.BAD_REQUEST, "model_export byte length mismatch")
        return payload

    # -- atomic edits and branching history

    def apply(
        self, session: SessionId, edits: Sequence[EditOp], *,
        if_rev: int | None = None, validate: bool = False,
    ) -> ResponseBodyEditResults:
        """Apply one atomic edit list, or validate it without publication.

        `ProtocolError.op_index` identifies a failed operation. No preceding
        operation in a refused list is published.
        """
        command = EditValidate if validate else EditApply
        body = self.send(command(
            session=session, if_rev=self.require_revision(session, if_rev),
            edits=list(edits),
        ))
        if not isinstance(body, ResponseBodyEditResults):
            raise ProtocolError(ErrorCode.BAD_REQUEST, f"{command.CMD} answered {body!r}")
        return body

    def undo(self, session: SessionId, *, if_rev: int | None = None) -> None:
        self.send(Undo(session=session, if_rev=self.require_revision(session, if_rev)))

    def redo(self, session: SessionId, *, if_rev: int | None = None) -> None:
        self.send(Redo(session=session, if_rev=self.require_revision(session, if_rev)))

    def goto(
        self, session: SessionId, revision: int, *, if_rev: int | None = None,
    ) -> None:
        self.send(EditGoto(
            session=session, revision=revision,
            if_rev=self.require_revision(session, if_rev),
        ))

    def history(self, session: SessionId) -> ResponseBodyEditHistory:
        body = self.send(EditHistoryGet(session=session))
        if not isinstance(body, ResponseBodyEditHistory):
            raise ProtocolError(ErrorCode.BAD_REQUEST, f"edit_history_get answered {body!r}")
        return body

    # -- rendering

    def preview(
        self,
        session: SessionId,
        pose: Mapping[str, float] | None = None,
        size: tuple[int, int] | None = None,
        camera: Camera | None = None,
        out: str | os.PathLike[str] | None = None,
    ) -> bytes:
        """Render one frame of `session` and return the PNG.

        `camera` absent frames the editor's default; what any tab is looking at
        is never consulted, so this answers the same whoever else is connected.
        `out` writes the bytes here as well as returning them.
        """
        body, payload = self.send_with(
            Preview(
                session=session,
                pose=[ParamPose(param=p, value=v) for p, v in (pose or {}).items()],
                size=size,
                camera=camera,
            )
        )
        if not isinstance(body, ResponseBodyPreview) or payload is None:
            raise ProtocolError(
                ErrorCode.BAD_REQUEST, f"preview answered {body!r} with no png"
            )
        if out is not None:
            Path(_absolute(out)).write_bytes(payload)
        return payload

    # -- extensions

    def extension_set(self, session: SessionId, key: ExtensionKey, value: Any) -> None:
        """File `value` under `key`, replacing whatever was there.

        `bytes` (or a `bytearray`) is a byte extension and travels attached;
        anything else is JSON and travels inline. The Python type decides,
        because that is the distinction a caller already made when it built
        the value.

        `catchlight.` is the format's own prefix and is refused.
        """
        if isinstance(value, (bytes, bytearray)):
            self.send_with(
                CommandExtensionSet(session=session, key=key, value=ExtensionSetBytes()),
                {"value": bytes(value)},
            )
        else:
            self.send_with(
                CommandExtensionSet(
                    session=session, key=key, value=ExtensionSetJson(value=value)
                )
            )

    def extension_get(self, session: SessionId, key: ExtensionKey) -> Any:
        """The value filed under `key`: the JSON it holds, or its `bytes`.

        The same split as `extension_set`, in reverse — so a value written
        here comes back as what was written.
        """
        body, payload = self.send_with(ExtensionGet(session=session, key=key))
        if not isinstance(body, ResponseBodyExtension):
            raise ProtocolError(ErrorCode.BAD_REQUEST, f"extension_get answered {body!r}")
        if isinstance(body.value, ExtensionValueInfoJson):
            return body.value.value
        if payload is None:
            raise ProtocolError(
                ErrorCode.BAD_REQUEST, f"{key}: a bytes extension came back with no bytes"
            )
        return payload

    def extension_delete(self, session: SessionId, key: ExtensionKey) -> None:
        """Drop the extension filed under `key`. A key the model does not
        carry raises, rather than quietly doing nothing."""
        self.send(ExtensionDelete(session=session, key=key))

    def extensions(self, session: SessionId) -> list[ExtensionInfo]:
        """Every extension the model carries, in key order.

        A byte value is reported as its size and hash — the marker a feed
        compares — never its bytes; `extension_get` is what fetches those.
        """
        body = self.send(Extensions(session=session))
        if not isinstance(body, ResponseBodyExtensions):
            raise ProtocolError(ErrorCode.BAD_REQUEST, f"extensions answered {body!r}")
        return body.extensions


def _structure_attachments(
    structure: Mapping[str, Any] | str,
    textures: Mapping[str, str | os.PathLike[str]]
    | Iterable[tuple[str, str | os.PathLike[str]]],
    alpha: TextureAlpha,
) -> tuple[list[ImportTexture], dict[str, bytes]]:
    body = structure if isinstance(structure, str) else json.dumps(structure)
    pairs = textures.items() if isinstance(textures, Mapping) else textures
    declared: list[ImportTexture] = []
    attachments: dict[str, bytes] = {"structure": body.encode()}
    for texture, path in pairs:
        source = Path(_absolute(path))
        declared.append(ImportTexture(
            texture=TexId(texture), encoding=_encoding_of(source), alpha=alpha,
        ))
        attachments[f"texture:{texture}"] = source.read_bytes()
    return declared, attachments


def _encoding_of(path: Path) -> TextureEncoding:
    """How to read an image, from its suffix. The client's call now: the
    command carries the encoding as a field."""
    if path.suffix.lower() == ".tga":
        return TextureEncoding.TGA
    return TextureEncoding.PNG


def _texture_references(manifest: bytes, source: Path) -> list[str]:
    """Every `path` the manifest's textures name, verbatim.

    Verbatim is the point: the editor matches an attachment to a reference by
    the string the manifest spells, so normalising one here would name a
    texture the manifest never asked for.
    """
    try:
        parsed = json.loads(manifest)
    except ValueError as err:
        raise ProtocolError(ErrorCode.MANIFEST, f"{source}: {err}") from err
    textures = parsed.get("textures", []) if isinstance(parsed, dict) else []
    return [t["path"] for t in textures if isinstance(t, dict) and "path" in t]


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
