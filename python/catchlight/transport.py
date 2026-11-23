"""Transports that carry one editor request out and its reply back.

A failure once a request is on the wire raises rather than retries: the
request may already have been applied. The socket transport is unix-only.
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

# The socket counts the newline against this cap.
# The socket counts the newline against it, so an encoded line has one byte
# less than this to spend.
MAX_REQUEST_BYTES = 1024 * 1024

# The editor closes connections idle for 30 s; reconnecting before a request
# avoids discovering a dead connection mid-request, which cannot be retried.
_SERVER_IDLE_CLOSE = 30.0
_IDLE_RECONNECT = 20.0


class TransportError(RuntimeError):
    """A connection failed, or answered something that is not a reply."""


@runtime_checkable
class Transport(Protocol):

    def request(self, command_wire: dict[str, Any]) -> dict[str, Any]:
        """Send one command and block until its reply arrives."""
        ...

    def request_with(
        self,
        command_wire: dict[str, Any],
        attachments: Mapping[str, bytes] | None = None,
    ) -> tuple[dict[str, Any], bytes | None]:
        """Send one command with its attachments; return the reply and payload."""
        ...

    def close(self) -> None:
        """Release the connection. Calling it twice is not an error."""
        ...


@runtime_checkable
class ByteTransport(Transport, Protocol):
    """A transport that can fetch a model or texture as bytes."""

    def get_clm(self, session: int) -> bytes:
        ...

    def get_texture(self, session: int, texture: str) -> bytes:
        """One texture's bytes, in whatever encoding the model stores."""
        ...


# ----------------------------------------------------------------- unix socket


class UnixSocketTransport:
    """The editor's Unix socket: one JSON request per line, one reply back."""

    def __init__(
        self,
        path: str | os.PathLike[str],
        *,
        timeout: float = _SERVER_IDLE_CLOSE,
        reconnect_after: float = _IDLE_RECONNECT,
    ) -> None:
        self.path = os.fspath(path)
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
        """The editor shares this filesystem, so bytes travel as temp files."""
        wants_payload = _carries_payload(command_wire)
        if not attachments and not wants_payload:
            return self._line(command_wire, {}), None
        with tempfile.TemporaryDirectory(prefix="catchlight-") as scratch:
            scratch = Path(scratch)
            siblings: dict[str, Any] = {}
            if attachments:
                files = {}
                for name, data in attachments.items():
                    # Attachment names may contain `/`, so files get opaque names.
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
        """Age is checked before a request because a failed one may have applied."""
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
        """Skip any event line; the socket answers requests in order."""
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
    row = COMMAND_BYTES.get(str(command_wire.get("cmd", "")))
    return row is not None and row.payload


def _encode_request(
    request_id: int,
    command_wire: dict[str, Any],
    siblings: Mapping[str, Any] | None = None,
) -> bytes:
    """The cap is checked against the socket's line, newline included."""
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
    """Counts dials so tests can tell a reused connection from a remade one."""

    def __init__(
        self, host: str, port: int, timeout: float, dialed: Callable[[], None]
    ) -> None:
        super().__init__(host, port, timeout=timeout)
        self._dialed = dialed

    def connect(self) -> None:
        super().connect()
        self._dialed()


class HttpTransport:
    """The editor's HTTP door: one `POST /request` per command."""

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
        """Multipart when there are attachments; a payload reply is the body,
        with the JSON reply in `X-Catchlight-Reply`."""
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
        """A non-2xx status is the HTTP door refusing; editor refusals are 200s."""
        conn = self._connection()
        headers = {"Authorization": f"Bearer {self.token}"}
        if content_type is not None:
            headers["Content-Type"] = content_type
        try:
            conn.request(method, path, body=body, headers=headers)
            response = conn.getresponse()
            answer = response.read()
        except (OSError, http.client.HTTPException) as err:
            # Never retried: a request that failed on the wire may have applied.
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
        """Age is checked before a request because a failed one may have applied."""
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
    """Hand-rolled to match the editor's strict parser (CRLF, closing boundary)."""
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
    """The bearer token travels in clear text, so only loopback is allowed."""
    parts = urllib.parse.urlsplit(url)
    if parts.scheme != "http":
        raise TransportError(f"{url}: the editor's http door is plain http")
    host = parts.hostname or ""
    if host not in _LOOPBACK:
        raise TransportError(f"{url}: the editor is reachable on loopback only, not {host!r}")
    return host, parts.port or 80
