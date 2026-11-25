"""A blocking client over one transport.

Paths are made absolute before they are sent, because a relative key would
resolve against the server's store root rather than this script's directory.
"""

from __future__ import annotations

import json
import os
from collections.abc import Iterable, Mapping, Sequence
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, TypeVar

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
    ExtensionRef,
    ExtensionWarningInfo,
    ExtensionWarnings,
    ClipInfo,
    ClipList,
    ControlMappingInfo,
    ExpressionDelete,
    ExpressionGroup,
    ExpressionInfo,
    ExpressionList,
    ExpressionSet,
    ExpressionSetting,
    HumanoidGet,
    HumanoidMap,
    HumanoidUnmap,
    VisemeAddStandard,
    ResponseBodyClips,
    ResponseBodyExpression,
    ResponseBodyExpressionDeleted,
    ResponseBodyExpressions,
    ResponseBodyExtensionWarnings,
    ResponseBodyHumanoid,
    ResponseBodyHumanoidMapped,
    ResponseBodyHumanoidUnmapped,
    ResponseBodyVisemesAdded,
    ExtensionSetBytes,
    ExtensionSetJson,
    ExtensionSetMaintained,
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
    ExtensionValueInfoMaintained,
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
    ResponseBodyReview,
    ResponseBodyReviewAcked,
    ResponseBodyReviewClaimed,
    ResponseBodyReviewFetched,
    ResponseBodyReviewImage,
    ResponseBodyReviewPending,
    ResponseBodyReviewThread,
    ResponseBodyVersion,
    ResponseBodyVersions,
    Review,
    ReviewAuthor,
    ReviewAuthorAgent,
    ReviewAuthorHuman,
    ReviewAck,
    ReviewClaim,
    ReviewCommentAdd,
    ReviewFetch,
    ReviewGet,
    ReviewImageAdd,
    ReviewImageGet,
    ReviewIdentity,
    ReviewPoll,
    ReviewRelease,
    ReviewSubmit,
    ReviewTarget,
    ReviewTargetModel,
    ReviewThread,
    ReviewThreadAdd,
    ReviewThreadSet,
    ThreadId,
    ThreadStatus,
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
    VersionGet,
    VersionInfo,
    VersionList,
    VersionOpen,
    VersionRestore,
    VersionSave,
    parse_reply,
)
from .transport import ByteTransport, Transport

__all__ = ["AGENT", "Client", "Maintained", "ProtocolError"]

_Body = TypeVar("_Body")

AGENT = ReviewIdentity(id="agent", name="agent")
"""The agent a review call acts as unless told otherwise."""


@dataclass(frozen=True)
class Maintained:
    """A maintained extension: a payload, the version of its format, and the
    reference table the editor keeps current as the model's Ids change.

    `payload` is any JSON value, or `bytes`. A reference the model lacks is
    kept, never refused.
    """

    version: int
    payload: Any
    refs: list[ExtensionRef] = field(default_factory=list)


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
        """Raises `ProtocolError` if the editor refused the command."""
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
        return self._revisions.get(session)

    def _record(self, command: Command, reply: ReplyOk) -> None:
        """A fork's outer revision belongs to the new session, not its source."""
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
        """An explicit guard, else the last captured revision.

        A stale known revision is kept so the server refuses the write.
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
        """`name` is a title only; nothing addresses it."""
        return _session(self.send(SessionNew(name=name)))

    def open(self, path: str | os.PathLike[str]) -> SessionId:
        """Over HTTP `path` is a key in the server's own store instead."""
        return _session(self.send(SessionOpen(path=_absolute(path))))

    def close_session(self, session: SessionId) -> None:
        """Unsaved edits are lost."""
        self.send(SessionClose(session=session))

    # -- output

    def save(self, session: SessionId, key: str | None = None) -> str:
        return _saved(self.send(Save(session=session, path=key)))

    def save_to(self, session: SessionId, path: str | os.PathLike[str]) -> str:
        """Over HTTP the model is fetched and written locally."""
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
        """The new session has no save path, unlike `open`."""
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
        """Binary extensions need a CLM source; JSON lacks their payloads."""
        declared, attachments = _structure_attachments(structure, textures, alpha)
        body, _ = self.send_with(
            SessionNew(name=name, source=SessionSourceJson(textures=declared)),
            attachments,
        )
        return _session(body)

    def from_manifest(
        self, manifest_path: str | os.PathLike[str], *, name: str | None = None,
    ) -> SessionId:
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
        """Existing IDs must not collide with the installed roots."""
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
        body = self.send(SessionFork(
            session=session, if_rev=self.require_revision(session, if_rev), name=name,
        ))
        if not isinstance(body, ResponseBodySessionFork):
            raise ProtocolError(ErrorCode.BAD_REQUEST, f"session_fork answered {body!r}")
        return body.session

    def export_model(self, session: SessionId, *, if_rev: int | None = None) -> bytes:
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
        """`ProtocolError.op_index` identifies a failed operation."""
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
        """`camera` absent frames the editor's default, not any tab's view."""
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
        """`bytes` becomes a byte extension, a `Maintained` a maintained one;
        anything else is JSON.

        The `catchlight.` prefix is reserved and refused.
        """
        if isinstance(value, Maintained):
            inline = not isinstance(value.payload, (bytes, bytearray))
            self.send_with(
                CommandExtensionSet(
                    session=session,
                    key=key,
                    value=ExtensionSetMaintained(
                        version=value.version,
                        refs=list(value.refs),
                        value=value.payload if inline else None,
                    ),
                ),
                None if inline else {"value": bytes(value.payload)},
            )
        elif isinstance(value, (bytes, bytearray)):
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
        """A JSON value, `bytes`, or a `Maintained` with its reference table."""
        body, payload = self.send_with(ExtensionGet(session=session, key=key))
        if not isinstance(body, ResponseBodyExtension):
            raise ProtocolError(ErrorCode.BAD_REQUEST, f"extension_get answered {body!r}")
        if isinstance(body.value, ExtensionValueInfoJson):
            return body.value.value
        if isinstance(body.value, ExtensionValueInfoMaintained):
            if body.value.value is None and payload is None:
                raise ProtocolError(
                    ErrorCode.BAD_REQUEST, f"{key}: a byte payload came back with no bytes"
                )
            return Maintained(
                version=body.value.version,
                payload=body.value.value if body.value.value is not None else payload,
                refs=list(body.value.refs),
            )
        if payload is None:
            raise ProtocolError(
                ErrorCode.BAD_REQUEST, f"{key}: a bytes extension came back with no bytes"
            )
        return payload

    def extension_delete(self, session: SessionId, key: ExtensionKey) -> None:
        """Raises if the model does not carry `key`."""
        self.send(ExtensionDelete(session=session, key=key))

    def extensions(self, session: SessionId) -> list[ExtensionInfo]:
        body = self.send(Extensions(session=session))
        if not isinstance(body, ResponseBodyExtensions):
            raise ProtocolError(ErrorCode.BAD_REQUEST, f"extensions answered {body!r}")
        return body.extensions

    def extension_warnings(self, session: SessionId) -> list[ExtensionWarningInfo]:
        """Every finding about the maintained extensions; never a refusal."""
        body = self.send(ExtensionWarnings(session=session))
        if not isinstance(body, ResponseBodyExtensionWarnings):
            raise ProtocolError(ErrorCode.BAD_REQUEST, f"extension_warnings answered {body!r}")
        return body.warnings

    # -- official extensions

    def humanoid(self, session: SessionId) -> ResponseBodyHumanoid:
        """The control mappings, and every standard control."""
        return self._expect(HumanoidGet(session=session), ResponseBodyHumanoid)

    def humanoid_map(
        self,
        session: SessionId,
        control: str,
        param: ParamId,
        *,
        stops: tuple[float, float, float] | None = None,
        nonstandard: bool = False,
    ) -> ResponseBodyHumanoidMapped:
        """Without `stops`, a two-sided control spans the param's min, default
        and max, and a one-sided one rests at the default and reaches the end
        of the range farther from it.
        A control outside the standard list needs `nonstandard`."""
        return self._expect(
            HumanoidMap(
                session=session,
                control=control,
                param=param,
                stops=stops,
                nonstandard=nonstandard,
            ),
            ResponseBodyHumanoidMapped,
        )

    def humanoid_unmap(self, session: SessionId, control: str) -> ControlMappingInfo:
        """Returns the removed mapping; the last one takes the extension."""
        body = self._expect(
            HumanoidUnmap(session=session, control=control), ResponseBodyHumanoidUnmapped
        )
        return body.mapping

    def clips(self, session: SessionId) -> list[ClipInfo]:
        """The model's clips with what each lane drives, in the model's order."""
        return self._expect(ClipList(session=session), ResponseBodyClips).clips

    def expressions(self, session: SessionId) -> ResponseBodyExpressions:
        """Both groups with their settings, and the standard names."""
        return self._expect(ExpressionList(session=session), ResponseBodyExpressions)

    def expression_set(
        self,
        session: SessionId,
        name: str,
        settings: Sequence[ExpressionSetting],
        *,
        group: ExpressionGroup = ExpressionGroup.EXPRESSION,
        nonstandard: bool = False,
    ) -> ResponseBodyExpression:
        """Creates or replaces an expression, or a viseme with `group`, its
        settings kept in the order given."""
        return self._expect(
            ExpressionSet(
                session=session,
                group=group,
                name=name,
                settings=list(settings),
                nonstandard=nonstandard,
            ),
            ResponseBodyExpression,
        )

    def expression_delete(
        self,
        session: SessionId,
        name: str,
        *,
        group: ExpressionGroup = ExpressionGroup.EXPRESSION,
    ) -> ExpressionInfo:
        """Returns the removed member; the last of both groups takes the
        extension."""
        body = self._expect(
            ExpressionDelete(session=session, group=group, name=name),
            ResponseBodyExpressionDeleted,
        )
        return body.expression

    def viseme_add_standard(
        self, session: SessionId, *, replace: bool = False
    ) -> ResponseBodyVisemesAdded:
        """Adds the standard visemes after the existing ones; an existing one
        of a standard name is skipped, or replaced with `replace`."""
        return self._expect(
            VisemeAddStandard(session=session, replace=replace), ResponseBodyVisemesAdded
        )

    def _expect(self, command: Command, kind: type[_Body]) -> _Body:
        body = self.send(command)
        if not isinstance(body, kind):
            raise ProtocolError(ErrorCode.BAD_REQUEST, f"{command.CMD} answered {body!r}")
        return body

    # -- review

    def review(self, session: SessionId) -> Review:
        """Every thread, and the agents attending to the session."""
        body = self.send(ReviewGet(session=session))
        if not isinstance(body, ResponseBodyReview):
            raise ProtocolError(ErrorCode.BAD_REQUEST, f"review_get answered {body!r}")
        return body.review

    def review_open(
        self,
        session: SessionId,
        body: str = "",
        *,
        target: ReviewTarget | None = None,
        agent: ReviewIdentity | None = AGENT,
        pose: Sequence[ParamPose] = (),
        attach: Sequence[ReviewAttachment] = (),
        key: str | None = None,
        version: int | None = None,
    ) -> ReviewThread:
        """Start a thread, on the whole model unless `target` names something.

        `agent=None` writes as the person, whose comments wait for `review_send`.
        `attach` is up to four PNG, JPEG or WebP images, each a path or the
        file's bytes; with one, `body` may be empty. Repeating the call with
        the same `key` returns the thread it opened and opens nothing.
        `version` points the comment at a saved version, such as the fix.
        """
        return _thread(
            self.send(
                ReviewThreadAdd(
                    session=session,
                    target=target if target is not None else ReviewTargetModel(),
                    author=_author(agent),
                    body=body,
                    pose=list(pose),
                    images=self._review_images(session, attach),
                    key=key,
                    version=version,
                )
            )
        )

    def review_reply(
        self,
        session: SessionId,
        thread: ThreadId,
        body: str = "",
        *,
        agent: ReviewIdentity | None = AGENT,
        attach: Sequence[ReviewAttachment] = (),
        key: str | None = None,
        version: int | None = None,
        save_version: str | bool = False,
    ) -> ReviewThread:
        """An agent's reply releases its claim on the thread.

        `attach`, `key` and `version` are as for `review_open`.
        `save_version` saves the model as a new version first, labelled
        when it is a string, and points the reply at it; with `key`, a
        retry saves nothing twice.
        """
        if save_version is not False:
            version = self.version_save(
                session,
                label=save_version if isinstance(save_version, str) else None,
                agent=agent,
                key=key,
            ).version
        return _thread(
            self.send(
                ReviewCommentAdd(
                    session=session,
                    thread=thread,
                    author=_author(agent),
                    body=body,
                    images=self._review_images(session, attach),
                    key=key,
                    version=version,
                )
            )
        )

    def _review_images(
        self, session: SessionId, attach: Sequence[ReviewAttachment]
    ) -> list[int]:
        images: list[int] = []
        for source in attach:
            data = (
                bytes(source)
                if isinstance(source, (bytes, bytearray))
                else Path(_absolute(source)).read_bytes()
            )
            body, _ = self.send_with(ReviewImageAdd(session=session), {"image": data})
            if not isinstance(body, ResponseBodyReviewImage):
                raise ProtocolError(ErrorCode.BAD_REQUEST, f"review_image_add answered {body!r}")
            images.append(body.image.image)
        return images

    def review_image(self, session: SessionId, image: int) -> bytes:
        """The bytes of an image a comment lists."""
        _, payload = self.send_with(ReviewImageGet(session=session, image=image))
        if payload is None:
            raise ProtocolError(
                ErrorCode.BAD_REQUEST, f"review image {image} came back with no bytes"
            )
        return payload

    def review_send(self, session: SessionId) -> None:
        """Hand the person's unsent comments to the agent."""
        self.send(ReviewSubmit(session=session))

    def review_poll(self, session: SessionId, *, agent: ReviewIdentity = AGENT) -> int:
        """How many threads wait for `agent`; also its listening heartbeat."""
        body = self.send(ReviewPoll(session=session, agent=agent))
        if not isinstance(body, ResponseBodyReviewPending):
            raise ProtocolError(ErrorCode.BAD_REQUEST, f"review_poll answered {body!r}")
        return body.pending

    def review_fetch(
        self, session: SessionId, *, agent: ReviewIdentity = AGENT
    ) -> ResponseBodyReviewFetched:
        """The threads waiting for `agent`, the new comments still `sent`.

        Nothing is delivered until `review_ack(batch)`: fetching again first
        returns the same comments.
        """
        body = self.send(ReviewFetch(session=session, agent=agent))
        if not isinstance(body, ResponseBodyReviewFetched):
            raise ProtocolError(ErrorCode.BAD_REQUEST, f"review_fetch answered {body!r}")
        return body

    def review_ack(
        self, session: SessionId, batch: str, *, agent: ReviewIdentity = AGENT
    ) -> ResponseBodyReviewAcked:
        """Deliver a fetched batch and claim its threads where nobody else does.

        Raises `BATCH_STALE` when the batch changed since it was fetched.
        """
        body = self.send(ReviewAck(session=session, agent=agent, batch=batch))
        if not isinstance(body, ResponseBodyReviewAcked):
            raise ProtocolError(ErrorCode.BAD_REQUEST, f"review_ack answered {body!r}")
        return body

    def review_claim(
        self, session: SessionId, threads: Sequence[ThreadId], *, agent: ReviewIdentity = AGENT
    ) -> int:
        """Take or renew claims on `threads`; the lease in seconds.

        Raises `THREAD_CLAIMED` when another agent holds one.
        """
        body = self.send(ReviewClaim(session=session, agent=agent, threads=list(threads)))
        if not isinstance(body, ResponseBodyReviewClaimed):
            raise ProtocolError(ErrorCode.BAD_REQUEST, f"review_claim answered {body!r}")
        return body.lease_seconds

    def review_release(
        self, session: SessionId, threads: Sequence[ThreadId], *, agent: ReviewIdentity = AGENT
    ) -> None:
        """Give up `agent`'s own claims on `threads`."""
        self.send(ReviewRelease(session=session, agent=agent, threads=list(threads)))

    def resolve(
        self, session: SessionId, thread: ThreadId, *, resolved: bool = True
    ) -> ReviewThread:
        """The person's call; an agent leaves threads open."""
        status = ThreadStatus.RESOLVED if resolved else ThreadStatus.OPEN
        return _thread(
            self.send(ReviewThreadSet(session=session, thread=thread, status=status))
        )


    # -- versions

    def version_save(
        self,
        session: SessionId,
        *,
        label: str | None = None,
        summary: str | None = None,
        agent: ReviewIdentity | None = AGENT,
        key: str | None = None,
    ) -> VersionInfo:
        """Keep the model as it is now as the next numbered version.

        Moves no revision and saves no file. `agent=None` saves as the
        person. Repeating the call with the same `key` and label returns the
        version it saved and saves nothing.
        """
        return _version(self.send(VersionSave(
            session=session, label=label, summary=summary, author=_author(agent), key=key,
        )))

    def versions(self, session: SessionId) -> ResponseBodyVersions:
        """The session's versions, oldest first, and how long they are kept."""
        body = self.send(VersionList(session=session))
        if not isinstance(body, ResponseBodyVersions):
            raise ProtocolError(ErrorCode.BAD_REQUEST, f"version_list answered {body!r}")
        return body

    def version_bytes(self, session: SessionId, version: int) -> bytes:
        """A version's `.clm`, byte for byte as it was saved."""
        body, payload = self.send_with(VersionGet(session=session, version=version))
        info = _version(body)
        if payload is None or len(payload) != info.byte_length:
            raise ProtocolError(
                ErrorCode.BAD_REQUEST, f"version {version} came back without its bytes"
            )
        return payload

    def version_restore(
        self, session: SessionId, version: int, *, if_rev: int | None = None
    ) -> VersionInfo:
        """Replace the model with a version's as one undoable edit."""
        return _version(self.send(VersionRestore(session=session, version=version, if_rev=if_rev)))

    def version_open(self, session: SessionId, version: int) -> SessionId:
        """A read-only session showing the version; it refuses every edit."""
        return _session(self.send(VersionOpen(session=session, version=version)))


ReviewAttachment = bytes | bytearray | str | os.PathLike[str]


def _version(body: ResponseBody) -> VersionInfo:
    if not isinstance(body, ResponseBodyVersion):
        raise ProtocolError(ErrorCode.BAD_REQUEST, f"expected a version, got {body!r}")
    return body.version


def _author(agent: ReviewIdentity | None) -> ReviewAuthor:
    if agent is None:
        return ReviewAuthorHuman()
    return ReviewAuthorAgent(id=agent.id, name=agent.name)


def _thread(body: ResponseBody) -> ReviewThread:
    if not isinstance(body, ResponseBodyReviewThread):
        raise ProtocolError(ErrorCode.BAD_REQUEST, f"expected a review thread, got {body!r}")
    return body.thread


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
    if path.suffix.lower() == ".tga":
        return TextureEncoding.TGA
    return TextureEncoding.PNG


def _texture_references(manifest: bytes, source: Path) -> list[str]:
    """Verbatim: the editor matches attachments by the manifest's spelling."""
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
