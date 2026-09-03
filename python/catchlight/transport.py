"""How a request reaches the editor, and how its reply comes back.

One interface, two doors. [`UnixSocketTransport`] speaks the newline-delimited
JSON the editor's socket speaks; [`HttpTransport`] speaks the same lines as
WebSocket text frames and carries bytes over HTTP beside them. Nothing here
knows what a command means: a transport moves one JSON object out and the
matching one back, and [`catchlight.client`] is what reads them.

Invariants this module enforces:

- **The correlation id is the transport's.** A caller hands over a command's
  wire object; the transport mints the `id`, puts `{"id": n, **command}` on the
  wire, and returns the reply carrying that id. Only a transport can tell a
  reply from a pushed event, so only a transport can do the matching.

- **A request is never sent twice.** The socket answers a request exactly once
  and a document command applied twice is a document nobody asked for, so a
  failure once a request is on the wire raises rather than retries. The only
  reconnect is *before* a request, on a connection old enough to be suspect.

- **The socket carries no events.** The editor pushes events to WebSocket
  clients only, so `events()` on the socket is always empty and a blocking
  caller learns a document moved from the `rev` on its own reply.

- **A frame is one request, and one request is at most a mebibyte.** Both doors
  cap a line at `MAX_REQUEST_BYTES`, so a line over it is refused here rather
  than closing the connection over there.

- **The HTTP door is loopback and unencrypted.** The bearer token travels in
  clear text, so the URL must name loopback; a remote host is refused rather
  than handed the token.

The socket transport is unix-only: it uses `AF_UNIX`, which Windows Python does
not offer here.
"""

from __future__ import annotations

import base64
import hashlib
import json
import os
import socket
import struct
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
from collections.abc import Iterator
from dataclasses import dataclass, field
from typing import Any, Protocol, runtime_checkable

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

# The editor closes a socket connection idle for 30 s (`SOCKET_IO_TIMEOUT`).
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
    """One request out, its reply back, plus whatever arrived unasked."""

    def request(self, command_wire: dict[str, Any]) -> dict[str, Any]:
        """Send one command and block until its reply arrives.

        `command_wire` is a command's `to_wire()`; the `id` is added here.
        """
        ...

    def events(self) -> list[dict[str, Any]]:
        """Every event queued since the last call, and drain the queue."""
        ...

    def close(self) -> None:
        """Release the connection. Calling it twice is not an error."""
        ...


@runtime_checkable
class ByteTransport(Transport, Protocol):
    """A transport whose editor is reached over the network rather than over a
    shared filesystem, so payloads travel as bytes.

    A client checks for this to decide whether a texture is a path the editor
    can open or bytes it has to stage first. The socket transport carries none:
    its editor reads the very filesystem the script is running on.
    """

    def put_file(self, key: str, data: bytes) -> None:
        """Stage `data` under the storage key a later command will name."""
        ...

    def get_clm(self, session: int) -> bytes:
        """The session's complete document, as `.clm` bytes."""
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
        with self._lock:
            line = _encode_request(self._mint_id(), command_wire)
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

    def events(self) -> list[dict[str, Any]]:
        """Always empty: the editor pushes events to WebSocket clients only."""
        return []

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


def _encode_request(request_id: int, command_wire: dict[str, Any]) -> bytes:
    """One request as the editor reads it: the correlation id, then the command.

    No newline — the socket adds one and a frame carries none — but the cap is
    checked against the line the socket would write, which is the stricter of
    the two by exactly that byte.
    """
    line = json.dumps({"id": request_id, **command_wire}, separators=(",", ":")).encode()
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


# ------------------------------------------------------------------ websocket

_WS_GUID = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
_OP_CONTINUATION = 0x0
_OP_TEXT = 0x1
_OP_BINARY = 0x2
_OP_CLOSE = 0x8
_OP_PING = 0x9
_OP_PONG = 0xA
_LOOPBACK = frozenset({"localhost", "127.0.0.1", "::1"})


@dataclass
class _Pending:
    """One caller blocked on one request id, and the reply the reader hands it."""

    done: threading.Event = field(default_factory=threading.Event)
    reply: dict[str, Any] | None = None


class HttpTransport:
    """The editor's HTTP door: `/ws` for messages, plain HTTP for bytes.

    A reader thread owns the socket's read half. It answers the editor's
    keepalive pings, queues events, and hands each reply to whichever caller is
    blocked on that id — without it an idle client would be pinged, say
    nothing, and be dropped after two intervals.
    """

    def __init__(
        self,
        url: str,
        token: str,
        *,
        timeout: float = 60.0,
    ) -> None:
        self.url = url.rstrip("/")
        self.token = token
        self._timeout = timeout
        self._base = _loopback_base(self.url)

        self._send_lock = threading.Lock()
        self._state_lock = threading.Lock()
        self._pending: dict[int, _Pending] = {}
        self._queued: list[dict[str, Any]] = []
        self._failure: str | None = None
        self._next_id = 0

        self._sock = _handshake(self._base, self.token)
        self._reader = threading.Thread(
            target=self._read_forever, name="catchlight-ws", daemon=True
        )
        self._reader.start()

    def __enter__(self) -> HttpTransport:
        return self

    def __exit__(self, *_exc: object) -> None:
        self.close()

    # -- messages

    def request(self, command_wire: dict[str, Any]) -> dict[str, Any]:
        with self._state_lock:
            if self._failure is not None:
                raise TransportError(self._failure)
            self._next_id += 1
            request_id = self._next_id
            pending = _Pending()
            self._pending[request_id] = pending
        try:
            self._send(_OP_TEXT, _encode_request(request_id, command_wire))
        except TransportError:
            with self._state_lock:
                self._pending.pop(request_id, None)
            raise
        answered = pending.done.wait(self._timeout)
        with self._state_lock:
            self._pending.pop(request_id, None)
            reply, failure = pending.reply, self._failure
        if reply is not None:
            return reply
        if failure is not None:
            raise TransportError(failure)
        if not answered:
            raise TransportError(f"no reply to request {request_id} within {self._timeout}s")
        raise TransportError("the editor closed the websocket")

    def events(self) -> list[dict[str, Any]]:
        with self._state_lock:
            drained, self._queued = self._queued, []
        return drained

    def close(self) -> None:
        sock = self._sock
        if sock is None:
            return
        try:
            self._send(_OP_CLOSE, struct.pack("!H", 1000))
        except TransportError:
            pass
        self._fail("the websocket is closed")
        try:
            sock.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass
        self._reader.join(timeout=2.0)
        try:
            sock.close()
        except OSError:
            pass
        self._sock = None

    # -- bytes

    def put_file(self, key: str, data: bytes) -> None:
        """`PUT /files/{key}`, which parks the bytes until the command that
        names the key reads them into a document."""
        quoted = urllib.parse.quote(key, safe="/")
        self._http("PUT", f"/files/{quoted}", body=data)

    def get_clm(self, session: int) -> bytes:
        return self._http("GET", f"/sessions/{session}/clm")

    def get_texture(self, session: int, texture: str) -> bytes:
        quoted = urllib.parse.quote(texture, safe="")
        return self._http("GET", f"/sessions/{session}/textures/{quoted}")

    def _http(self, method: str, path: str, body: bytes | None = None) -> bytes:
        request = urllib.request.Request(
            f"{self._base}{path}",
            data=body,
            method=method,
            headers={
                "Authorization": f"Bearer {self.token}",
                **({"Content-Type": "application/octet-stream"} if body is not None else {}),
            },
        )
        try:
            with urllib.request.urlopen(request, timeout=self._timeout) as response:
                return response.read()
        except urllib.error.HTTPError as err:
            detail = err.read().decode("utf-8", "replace").strip()
            raise TransportError(f"{method} {path}: {err.code} {err.reason}: {detail}") from err
        except OSError as err:
            raise TransportError(f"{method} {path}: {err}") from err

    # -- framing

    def _send(self, opcode: int, payload: bytes) -> None:
        """One whole frame, masked as a client must mask.

        Held under a lock because a frame interleaved with another frame's
        payload is not a frame, and the reader thread sends pongs.
        """
        sock = self._sock
        if sock is None:
            raise TransportError("the websocket is closed")
        header = bytearray([0x80 | opcode])
        length = len(payload)
        if length < 126:
            header.append(0x80 | length)
        elif length < 1 << 16:
            header.append(0x80 | 126)
            header += struct.pack("!H", length)
        else:
            header.append(0x80 | 127)
            header += struct.pack("!Q", length)
        mask = os.urandom(4)
        header += mask
        masked = bytes(byte ^ mask[i % 4] for i, byte in enumerate(payload))
        with self._send_lock:
            try:
                sock.sendall(bytes(header) + masked)
            except OSError as err:
                raise TransportError(f"{self.url}: the websocket write failed: {err}") from err

    def _read_forever(self) -> None:
        try:
            for opcode, payload in _frames(self._sock):
                if opcode == _OP_PING:
                    self._send(_OP_PONG, payload)
                elif opcode == _OP_PONG:
                    continue
                elif opcode == _OP_CLOSE:
                    break
                elif opcode == _OP_TEXT:
                    self._deliver(payload)
                elif opcode == _OP_BINARY:
                    # Not this protocol. The editor answers a binary frame with
                    # an error rather than sending one, so there is nothing to
                    # do here but ignore it.
                    continue
        except (TransportError, OSError, ValueError) as err:
            self._fail(f"{self.url}: the websocket failed: {err}")
            return
        self._fail("the editor closed the websocket")

    def _deliver(self, payload: bytes) -> None:
        message = _decode(payload)
        with self._state_lock:
            if message.get("reply") == "event":
                self._queued.append(message)
                return
            pending = self._pending.get(message.get("id"))
            if pending is None:
                # An answer to a request nobody is waiting for: a caller that
                # gave up, or a `Reply::Err { id: 0 }` for a line the editor
                # could not read at all. Neither has an owner to raise at.
                self._queued.append(message)
                return
            pending.reply = message
            pending.done.set()

    def _fail(self, why: str) -> None:
        """Wake every blocked caller with the same reason.

        Nothing is retried: a request whose connection died may or may not have
        been applied, and the caller is the only one who knows whether that is
        survivable.
        """
        with self._state_lock:
            if self._failure is None:
                self._failure = why
            waiting = list(self._pending.values())
            self._pending.clear()
        for pending in waiting:
            pending.done.set()


def _loopback_base(url: str) -> str:
    """The scheme and authority to reach, refusing anything but loopback.

    The bearer token is sent in clear text over a door that offers no TLS, so a
    URL naming a host somewhere else would be handing it out.
    """
    parts = urllib.parse.urlsplit(url)
    if parts.scheme != "http":
        raise TransportError(f"{url}: the editor's http door is plain http")
    host = parts.hostname or ""
    if host not in _LOOPBACK:
        raise TransportError(f"{url}: the editor is reachable on loopback only, not {host!r}")
    return f"http://{parts.netloc}"


def _handshake(base: str, token: str) -> socket.socket:
    """Open `/ws` and check the accept key, or raise.

    No `Origin` header: a browser always sends one and the editor checks it
    against an allowlist, but a client that sends none is a non-browser one and
    is let through on the token alone.
    """
    parts = urllib.parse.urlsplit(base)
    host, port = parts.hostname or "127.0.0.1", parts.port or 80
    key = base64.b64encode(os.urandom(16))
    try:
        sock = socket.create_connection((host, port), timeout=10.0)
    except OSError as err:
        raise TransportError(f"{base}: no editor listening: {err}") from err
    handshake = (
        f"GET /ws?token={urllib.parse.quote(token, safe='')} HTTP/1.1\r\n"
        f"Host: {parts.netloc}\r\n"
        "Upgrade: websocket\r\n"
        "Connection: Upgrade\r\n"
        f"Sec-WebSocket-Key: {key.decode()}\r\n"
        "Sec-WebSocket-Version: 13\r\n"
        "\r\n"
    )
    try:
        sock.sendall(handshake.encode())
        head, leftover = _read_head(sock)
    except OSError as err:
        sock.close()
        raise TransportError(f"{base}/ws: the handshake failed: {err}") from err
    if leftover:
        sock.close()
        raise TransportError(f"{base}/ws: the editor framed before the handshake ended")
    status = head.split("\r\n", 1)[0]
    if " 101 " not in status:
        sock.close()
        raise TransportError(f"{base}/ws: {status.strip()}")
    expected = base64.b64encode(hashlib.sha1(key + _WS_GUID).digest()).decode()
    if expected.lower() not in head.lower():
        sock.close()
        raise TransportError(f"{base}/ws: the accept key does not match the one sent")
    sock.settimeout(None)
    return sock


def _read_head(sock: socket.socket) -> tuple[str, bytes]:
    """The handshake response up to its blank line, and anything read past it.

    Bytes past the blank line would be the first frame, which this client never
    expects: the editor says nothing until it is asked.
    """
    buffer = bytearray()
    while b"\r\n\r\n" not in buffer:
        chunk = sock.recv(4096)
        if not chunk:
            raise OSError("the connection closed during the handshake")
        buffer += chunk
        if len(buffer) > 16 * 1024:
            raise OSError("handshake response is too large")
    head, _, rest = bytes(buffer).partition(b"\r\n\r\n")
    return head.decode("latin-1"), rest


def _frames(sock: socket.socket | None) -> Iterator[tuple[int, bytes]]:
    """Whole messages off the wire, reassembling any fragmentation.

    The editor sends none, but a continuation frame is two lines to handle and
    a message silently truncated is not.
    """
    if sock is None:
        return
    buffer = bytearray()
    kind: int | None = None
    parts = bytearray()
    while True:
        header = _recv_exactly(sock, buffer, 2)
        final = bool(header[0] & 0x80)
        opcode = header[0] & 0x0F
        if header[1] & 0x80:
            raise ValueError("the editor masked a frame, which a server may not do")
        length = header[1] & 0x7F
        if length == 126:
            (length,) = struct.unpack("!H", _recv_exactly(sock, buffer, 2))
        elif length == 127:
            (length,) = struct.unpack("!Q", _recv_exactly(sock, buffer, 8))
        if length > MAX_REQUEST_BYTES:
            raise ValueError(f"the editor sent a {length} byte frame")
        payload = _recv_exactly(sock, buffer, length)
        if opcode in (_OP_CLOSE, _OP_PING, _OP_PONG):
            yield opcode, bytes(payload)
            if opcode == _OP_CLOSE:
                return
            continue
        if opcode == _OP_CONTINUATION:
            if kind is None:
                raise ValueError("a continuation frame with nothing to continue")
        else:
            kind, parts = opcode, bytearray()
        parts += payload
        if final:
            yield kind, bytes(parts)
            kind = None


def _recv_exactly(sock: socket.socket, buffer: bytearray, want: int) -> bytes:
    while len(buffer) < want:
        chunk = sock.recv(max(4096, want - len(buffer)))
        if not chunk:
            raise OSError("the connection closed mid-frame")
        buffer += chunk
    taken = bytes(buffer[:want])
    del buffer[:want]
    return taken
