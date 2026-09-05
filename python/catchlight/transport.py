"""How a request reaches the editor, and how its reply comes back.

One interface, two doors. [`UnixSocketTransport`] speaks the newline-delimited
JSON the editor's socket speaks; [`HttpTransport`] puts the same JSON object in
a `POST /request` body and carries payloads over the same connection beside it.
Nothing here knows what a command *means*: a transport moves one JSON object
out and the matching one back, and [`catchlight.client`] is what reads them.
The one thing it does read is `COMMAND_BYTES`, which says what bytes a command
carries — that is a framing fact, and framing is all this module does.

Invariants this module enforces:

- **The correlation id is the transport's.** A caller hands over a command's
  wire object; the transport mints the `id`, puts `{"id": n, **command}` on the
  wire, and returns the reply carrying that id.

- **A request is never sent twice.** The editor answers a request exactly once
  and an edit applied twice is a model nobody asked for, so a
  failure once a request is on the wire raises rather than retries. The only
  reconnect is *before* a request, on a connection old enough to be suspect.

- **Bytes ride beside a command, and each door frames them its own way.**
  `request_with` takes the attachments a command declares and hands back the
  payload its reply carries. Over the socket both are files: the editor reads
  the very filesystem this script is on, so the attachments are written to
  temporary files named in a sibling `files` map and a payload is read back out
  of a temporary `out` path — nothing is uploaded. Over HTTP the request
  becomes `multipart/form-data` — a part named `request` holding the command
  and one part per attachment — and a payload comes back as the response body
  with its reply in `X-Catchlight-Reply`.

- **A temporary file outlives nothing.** Every file the socket transport writes
  for a request lives in one directory it makes and removes around that
  request, so a failure leaves nothing behind and two requests never see each
  other's bytes.

- **Neither door hears an event.** The editor pushes events over the WebSocket
  the browser tab holds, and this package holds none: a blocking caller learns
  its model moved from the `rev` on its own reply.

- **Over HTTP a status is the listener and a reply is the editor.** A status
  that is not a success describes this door only — no token, a body over the
  cap, a body that is not a request — and is raised here. Every refusal the
  editor itself decided arrives as a 200 carrying `err`, which is a reply like
  any other and the client's to raise on.

- **A request is at most a mebibyte.** Both doors cap one request at
  `MAX_REQUEST_BYTES`, so a request over it is refused here rather than sent to
  be refused there.

- **The HTTP door is loopback and unencrypted.** The bearer token travels in
  clear text, so the URL must name loopback; a remote host is refused rather
  than handed the token.

The socket transport is unix-only: it uses `AF_UNIX`, which Windows Python does
not offer here.
"""

from __future__ import annotations

import http.client
import json
import os
import socket
import tempfile
import threading
import time
import urllib.parse
import uuid
from collections.abc import Callable, Mapping
from pathlib import Path
from typing import Any, Protocol, runtime_checkable

from .protocol_gen import COMMAND_BYTES

__all__ = [
    "MAX_REQUEST_BYTES",
    "ByteTransport",
    "HttpTransport",
    "Transport",
    "TransportError",
    "UnixSocketTransport",
]

# The cap both doors put on one request, from `MAX_REQUEST_BYTES` in
# `crates/catchlight-editor-server/src/transport.rs` and its twin in `http.rs`.
# The socket counts the newline against it, so an encoded line has one byte
# less than this to spend.
MAX_REQUEST_BYTES = 1024 * 1024

# The editor closes a socket connection idle for 30 s (`SOCKET_IO_TIMEOUT`),
# and an HTTP one idle for 30 s too (`HTTP_IO_TIMEOUT`).
_SERVER_IDLE_CLOSE = 30.0
# So a connection this old is dropped and remade *before* the next request:
# the alternative is discovering it half way through one, which is the one
# failure this transport cannot retry.
_IDLE_RECONNECT = 20.0


class TransportError(RuntimeError):
    """A connection failed, or answered something that is not a reply.

    A refusal the editor *meant* — an unknown Id, a session that is not open —
    is a `ProtocolError` instead: it arrived as a reply, so the connection is
    fine.
    """


@runtime_checkable
class Transport(Protocol):
    """One request out, its reply back."""

    def request(self, command_wire: dict[str, Any]) -> dict[str, Any]:
        """Send one command and block until its reply arrives.

        `command_wire` is a command's `to_wire()`; the `id` is added here. For
        a command that carries no bytes — every command but the four in
        `COMMAND_BYTES`.
        """
        ...

    def request_with(
        self,
        command_wire: dict[str, Any],
        attachments: Mapping[str, bytes] | None = None,
    ) -> tuple[dict[str, Any], bytes | None]:
        """Send one command with the bytes it declares, and take back its reply
        and whatever bytes came with it.

        `attachments` is keyed by attachment name, as `COMMAND_BYTES` spells
        them. The payload is `None` unless the command's row says its reply
        carries one.
        """
        ...

    def close(self) -> None:
        """Release the connection. Calling it twice is not an error."""
        ...


@runtime_checkable
class ByteTransport(Transport, Protocol):
    """A transport that can hand back a model or a texture as bytes.

    A client checks for this to decide whether a save is a save or a fetch:
    the socket's editor writes the very filesystem the script is on, and an
    HTTP one writes its own. Nothing goes *in* this way — bytes a command
    needs travel with it.
    """

    def get_clm(self, session: int) -> bytes:
        """The session's complete model, as `.clm` bytes."""
        ...

    def get_texture(self, session: int, texture: str) -> bytes:
        """One texture's bytes, in whatever encoding the model stores."""
        ...


# ----------------------------------------------------------------- unix socket


class UnixSocketTransport:
    """The editor's Unix socket: one JSON request per line, one reply back.

    The socket is in a directory only its owner may enter, which is the whole
    of its access control — there is no token because there is no second party
    on this machine the file mode does not already stop.
    """

    def __init__(
        self,
        path: str | os.PathLike[str],
        *,
        timeout: float = _SERVER_IDLE_CLOSE,
        reconnect_after: float = _IDLE_RECONNECT,
    ) -> None:
        self.path = os.fspath(path)
        #: How many connections this transport has opened. A jump means the
        #: previous one was dropped rather than reused.
        self.connections = 0
        self._timeout = timeout
        self._reconnect_after = reconnect_after
        self._sock: socket.socket | None = None
        self._buffer = bytearray()
        self._last_used = 0.0
        self._next_id = 0
        self._lock = threading.Lock()

    def __enter__(self) -> UnixSocketTransport:
        return self

    def __exit__(self, *_exc: object) -> None:
        self.close()

    def request(self, command_wire: dict[str, Any]) -> dict[str, Any]:
        return self.request_with(command_wire)[0]

    def request_with(
        self,
        command_wire: dict[str, Any],
        attachments: Mapping[str, bytes] | None = None,
    ) -> tuple[dict[str, Any], bytes | None]:
        """One line, plus the sibling keys this socket carries beside it.

        Both halves are files. The editor is on this filesystem, so an
        attachment is written where it can read it and a payload is written
        where this can read it; the directory holding them is made and removed
        around the request.
        """
        wants_payload = _carries_payload(command_wire)
        if not attachments and not wants_payload:
            return self._line(command_wire, {}), None
        with tempfile.TemporaryDirectory(prefix="catchlight-") as scratch:
            scratch = Path(scratch)
            siblings: dict[str, Any] = {}
            if attachments:
                files = {}
                for name, data in attachments.items():
                    # The name is an attachment's, not a filename's, and may
                    # carry a `/` (`texture:images/face.png`), so the file is
                    # named by nothing but a counter.
                    path = scratch / f"attachment-{uuid.uuid4().hex}"
                    path.write_bytes(data)
                    files[name] = str(path)
                siblings["files"] = files
            out = scratch / "payload"
            if wants_payload:
                siblings["out"] = str(out)
            reply = self._line(command_wire, siblings)
            payload = out.read_bytes() if wants_payload and out.exists() else None
        return reply, payload

    def _line(self, command_wire: dict[str, Any], siblings: dict[str, Any]) -> dict[str, Any]:
        with self._lock:
            line = _encode_request(self._mint_id(), command_wire, siblings)
            sock = self._connection()
            try:
                sock.sendall(line + b"\n")
                return self._read_reply()
            except TransportError:
                self._drop()
                raise
            except OSError as err:
                self._drop()
                raise TransportError(
                    f"{self.path}: the editor went away mid-request: {err}"
                ) from err

    def close(self) -> None:
        with self._lock:
            self._drop()

    # -- connection

    def _mint_id(self) -> int:
        self._next_id += 1
        return self._next_id

    def _connection(self) -> socket.socket:
        """The live connection, remade first if it has been idle too long.

        Age is checked here rather than after a failure because a request that
        failed on the wire may already have been applied.
        """
        idle = time.monotonic() - self._last_used
        if self._sock is not None and idle >= self._reconnect_after:
            self._drop()
        if self._sock is None:
            self._sock = self._connect()
            self._buffer.clear()
        self._last_used = time.monotonic()
        return self._sock

    def _connect(self) -> socket.socket:
        sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        sock.settimeout(self._timeout)
        try:
            sock.connect(self.path)
        except OSError as err:
            sock.close()
            raise TransportError(f"{self.path}: no editor listening: {err}") from err
        self.connections += 1
        return sock

    def _drop(self) -> None:
        sock, self._sock = self._sock, None
        self._buffer.clear()
        if sock is not None:
            try:
                sock.close()
            except OSError:
                pass

    def _read_reply(self) -> dict[str, Any]:
        """The next line that is a reply.

        The socket answers in order, one reply per request, so the next line is
        this request's. The editor pushes no events here; a line that was one
        anyway is skipped rather than mistaken for an answer.
        """
        while True:
            message = _decode(self._read_line())
            if message.get("reply") == "event":
                continue
            return message

    def _read_line(self) -> bytes:
        sock = self._sock
        if sock is None:  # pragma: no cover - _connection just set it
            raise TransportError(f"{self.path}: not connected")
        while True:
            end = self._buffer.find(b"\n")
            if end >= 0:
                line = bytes(self._buffer[:end])
                del self._buffer[: end + 1]
                return line
            chunk = sock.recv(64 * 1024)
            if not chunk:
                raise TransportError(
                    f"{self.path}: the editor closed the connection before replying"
                )
            self._buffer.extend(chunk)


def _carries_payload(command_wire: dict[str, Any]) -> bool:
    """Does this command's reply carry bytes? `COMMAND_BYTES` is where that is
    written down, once, for every language."""
    row = COMMAND_BYTES.get(str(command_wire.get("cmd", "")))
    return row is not None and row.payload


def _encode_request(
    request_id: int,
    command_wire: dict[str, Any],
    siblings: Mapping[str, Any] | None = None,
) -> bytes:
    """One request as the editor reads it: the correlation id, then the command.

    `siblings` are the socket's own keys — `files` and `out` — which sit beside
    the command's fields rather than inside them.

    No newline — the socket adds one and a POST body carries none — but the cap
    is checked against the line the socket would write, which is the stricter of
    the two by exactly that byte.
    """
    line = json.dumps(
        {"id": request_id, **command_wire, **(siblings or {})}, separators=(",", ":")
    ).encode()
    if len(line) + 1 > MAX_REQUEST_BYTES:
        raise TransportError(
            f"request is {len(line) + 1} bytes, over the {MAX_REQUEST_BYTES} the editor reads"
        )
    return line


def _decode(line: bytes) -> dict[str, Any]:
    try:
        message = json.loads(line)
    except ValueError as err:
        raise TransportError(f"the editor sent a line that is not JSON: {err}") from err
    if not isinstance(message, dict):
        raise TransportError(f"the editor sent {type(message).__name__}, not an object")
    return message


# ----------------------------------------------------------------------- http

_LOOPBACK = frozenset({"localhost", "127.0.0.1", "::1"})


class _Connection(http.client.HTTPConnection):
    """`http.client`'s connection, counting the times it actually dials.

    It dials lazily and redials by itself once a response says the editor
    closed the connection, which is every response the editor sends today. The
    count is the only way from outside to tell a reused connection from a
    remade one.
    """

    def __init__(
        self, host: str, port: int, timeout: float, dialed: Callable[[], None]
    ) -> None:
        super().__init__(host, port, timeout=timeout)
        self._dialed = dialed

    def connect(self) -> None:
        super().connect()
        self._dialed()


class HttpTransport:
    """The editor's HTTP door: one `POST /request` per command, bytes beside it.

    A command is the very JSON object the socket carries, sent as the body of a
    POST and answered by one reply. There is no reader thread and no frame
    masking, because there is nothing to listen for: a route that answers a
    request pushes no event, and the events the editor does push are the
    browser tab's business, over a WebSocket this package does not open.

    Nothing is sent until the first request. A wrong token, or an editor that
    is not there, surfaces on that request rather than here.
    """

    def __init__(
        self,
        url: str,
        token: str,
        *,
        timeout: float = 60.0,
        reconnect_after: float = _IDLE_RECONNECT,
    ) -> None:
        self.url = url.rstrip("/")
        self.token = token
        #: How many times this transport has dialled. It rises once per request
        #: against an editor that closes every response, which is what the
        #: editor does today.
        self.connections = 0
        self._timeout = timeout
        self._reconnect_after = reconnect_after
        self._host, self._port = _loopback_authority(self.url)
        self._conn: _Connection | None = None
        self._last_used = 0.0
        self._next_id = 0
        self._lock = threading.Lock()

    def __enter__(self) -> HttpTransport:
        return self

    def __exit__(self, *_exc: object) -> None:
        self.close()

    # -- messages

    def request(self, command_wire: dict[str, Any]) -> dict[str, Any]:
        return self.request_with(command_wire)[0]

    def request_with(
        self,
        command_wire: dict[str, Any],
        attachments: Mapping[str, bytes] | None = None,
    ) -> tuple[dict[str, Any], bytes | None]:
        """One `POST /request`, with the bytes framed the way HTTP frames them.

        With attachments the body is `multipart/form-data`: a part named
        `request` holding the command, one part per attachment named for the
        attachment. Without them it is the JSON body it has always been —
        a command whose reply is bytes and whose request is not needs no form.

        A payload comes back as the response body under its own content type,
        and the reply rides in `X-Catchlight-Reply`, escaped to ASCII by the
        server so a header can hold it.
        """
        with self._lock:
            encoded = _encode_request(self._mint_id(), command_wire)
            if attachments:
                content_type, body = _multipart(encoded, attachments)
            else:
                content_type, body = "application/json", encoded
            headers, answer = self._exchange_seen(
                "POST", "/request", body, content_type
            )
        in_header = headers.get("X-Catchlight-Reply")
        if in_header is not None:
            return _decode(in_header.encode()), answer
        return _decode(answer), None

    def close(self) -> None:
        with self._lock:
            self._drop()

    # -- bytes

    def get_clm(self, session: int) -> bytes:
        return self._bytes("GET", f"/sessions/{session}/clm")

    def get_texture(self, session: int, texture: str) -> bytes:
        quoted = urllib.parse.quote(texture, safe="")
        return self._bytes("GET", f"/sessions/{session}/textures/{quoted}")

    def _bytes(self, method: str, path: str, body: bytes | None = None) -> bytes:
        kind = "application/octet-stream" if body is not None else None
        with self._lock:
            return self._exchange(method, path, body, kind)

    # -- connection

    def _mint_id(self) -> int:
        self._next_id += 1
        return self._next_id

    def _exchange(
        self, method: str, path: str, body: bytes | None, content_type: str | None
    ) -> bytes:
        return self._exchange_seen(method, path, body, content_type)[1]

    def _exchange_seen(
        self, method: str, path: str, body: bytes | None, content_type: str | None
    ) -> tuple[http.client.HTTPMessage, bytes]:
        """One round trip, with the lock held: the response's headers and body.

        A status that is not a success is this door refusing the request, so it
        raises here. A command the *editor* refused is a 200 whose body is an
        `err` reply, which is not this layer's to notice.
        """
        conn = self._connection()
        headers = {"Authorization": f"Bearer {self.token}"}
        if content_type is not None:
            headers["Content-Type"] = content_type
        try:
            conn.request(method, path, body=body, headers=headers)
            response = conn.getresponse()
            answer = response.read()
        except (OSError, http.client.HTTPException) as err:
            # Never retried: a request that failed on the wire may already have
            # been applied, and only the caller knows whether that is
            # survivable.
            self._drop()
            raise TransportError(
                f"{method} {path}: the editor went away mid-request: {err}"
            ) from err
        self._last_used = time.monotonic()
        if response.status // 100 != 2:
            detail = answer.decode("utf-8", "replace").strip()
            raise TransportError(f"{method} {path}: {response.status} {response.reason}: {detail}")
        return response.headers, answer

    def _connection(self) -> _Connection:
        """The connection to send on, remade first if it has been idle too long.

        Age is checked here rather than after a failure because a request that
        failed on the wire may already have been applied. Today the editor
        closes every response and `http.client` redials on the next request, so
        this rarely has anything to drop — it is what keeps the rule true if
        the editor ever keeps a connection alive.
        """
        idle = time.monotonic() - self._last_used
        if self._conn is not None and idle >= self._reconnect_after:
            self._drop()
        if self._conn is None:
            self._conn = _Connection(self._host, self._port, self._timeout, self._dialed)
        self._last_used = time.monotonic()
        return self._conn

    def _dialed(self) -> None:
        self.connections += 1

    def _drop(self) -> None:
        conn, self._conn = self._conn, None
        if conn is not None:
            try:
                conn.close()
            except OSError:
                pass


def _multipart(
    request: bytes, attachments: Mapping[str, bytes]
) -> tuple[str, bytes]:
    """The command and its bytes as one `multipart/form-data` body.

    Hand-rolled because the editor's parser is: CRLF everywhere, a name on
    every part, the closing boundary required. The boundary is random so it
    cannot appear in a part it is meant to delimit.
    """
    boundary = f"catchlight{uuid.uuid4().hex}"
    body = bytearray()
    for name, data in (("request", request), *attachments.items()):
        if '"' in name or "\r" in name or "\n" in name:
            raise TransportError(f"attachment name {name!r} cannot go in a part header")
        body += f"--{boundary}\r\n".encode()
        body += f'Content-Disposition: form-data; name="{name}"\r\n\r\n'.encode()
        body += data
        body += b"\r\n"
    body += f"--{boundary}--\r\n".encode()
    return f"multipart/form-data; boundary={boundary}", bytes(body)


def _loopback_authority(url: str) -> tuple[str, int]:
    """The host and port to reach, refusing anything but loopback.

    The bearer token is sent in clear text over a door that offers no TLS, so a
    URL naming a host somewhere else would be handing it out.
    """
    parts = urllib.parse.urlsplit(url)
    if parts.scheme != "http":
        raise TransportError(f"{url}: the editor's http door is plain http")
    host = parts.hostname or ""
    if host not in _LOOPBACK:
        raise TransportError(f"{url}: the editor is reachable on loopback only, not {host!r}")
    return host, parts.port or 80
