"""Launching an editor server, or connecting to one already running.

A launched server's socket lives in a fresh `0700` temp directory, which is
its only access control. Unix-only, because the socket is.
"""

from __future__ import annotations

import collections
import os
import shutil
import subprocess
import tempfile
import threading
import time
from pathlib import Path

from .client import Client, ProtocolError
from .protocol_gen import SessionList
from .transport import HttpTransport, TransportError, UnixSocketTransport

__all__ = ["LaunchedServer", "ServerError", "connect", "launch"]

BINARY_NAME = "catchlight-editor-server"
BINARY_ENV = "CATCHLIGHT_EDITOR_SERVER"

_PREFIX = f"{BINARY_NAME}: "
_POLL = 0.05
# Seconds a terminated server has to exit before it is killed.
_GRACE = 3.0


class ServerError(RuntimeError):
    """A server could not be started, did not come up, or is not there."""


def launch(
    *,
    binary: str | os.PathLike[str] | None = None,
    store: str | os.PathLike[str] | None = None,
    model: str | os.PathLike[str] | None = None,
    http: str | None = None,
    timeout: float = 10.0,
) -> LaunchedServer:
    """Start an editor on a private socket and wait until it answers.

    `binary` absent: `$CATCHLIGHT_EDITOR_SERVER`, the bundled server, `PATH`,
    then this workspace's `target/debug` build. `http` like `127.0.0.1:0`
    also serves the HTTP door; port 0 picks a free port.
    """
    argv = [_find_binary(binary)]
    directory = Path(tempfile.mkdtemp(prefix="catchlight-server-"))
    try:
        socket_path = directory / "server.sock"
        argv += ["--socket", str(socket_path)]
        argv += ["--store", str(Path(store).absolute() if store else Path.cwd())]
        if http is not None:
            argv += ["--http", http]
        if model is not None:
            argv.append(str(model))
        return LaunchedServer(
            argv=argv,
            socket_path=socket_path,
            directory=directory,
            timeout=timeout,
            wants_http=http is not None,
        )
    except BaseException:
        shutil.rmtree(directory, ignore_errors=True)
        raise


def connect(url: str, token: str) -> Client:
    """Connect over HTTP to a server someone else runs; never stops it."""
    return Client(HttpTransport(url, token))


class LaunchedServer:

    def __init__(
        self,
        *,
        argv: list[str],
        socket_path: Path,
        directory: Path,
        timeout: float,
        wants_http: bool = False,
    ) -> None:
        self.socket_path = socket_path
        self.directory = directory
        self._argv = argv
        self._timeout = timeout
        self._wants_http = wants_http
        self._process: subprocess.Popen[bytes] | None = None
        self._stderr = _StderrTail()
        self._client: Client | None = None
        self.http_url: str | None = None
        self.token: str | None = None
        self._start()

    def __enter__(self) -> LaunchedServer:
        return self

    def __exit__(self, *_exc: object) -> None:
        self.stop()

    @property
    def pid(self) -> int | None:
        return None if self._process is None else self._process.pid

    def client(self) -> Client:
        """One client per server, so its revisions are one running account."""
        if self._process is None:
            raise ServerError("this server has been stopped")
        if self._client is None:
            self._client = Client(UnixSocketTransport(self.socket_path))
        return self._client

    def connect(self) -> Client:
        """A second client over the HTTP door; requires `launch(http=...)`."""
        if self.http_url is None or self.token is None:
            raise ServerError("this server was launched without an http address")
        return Client(HttpTransport(self.http_url, self.token))

    def healthy(self) -> bool:
        if self._process is None or self._process.poll() is not None:
            return False
        try:
            self.client().send(SessionList())
        except (ProtocolError, ServerError, TransportError):
            return False
        return True

    def stderr(self) -> str:
        return self._stderr.text()

    def stop(self) -> None:
        """Idempotent; also removes the socket directory."""
        self._release_client()
        self._end_process()
        shutil.rmtree(self.directory, ignore_errors=True)

    def restart(self) -> None:
        """The new process holds none of the old sessions."""
        self._release_client()
        self._end_process()
        self._start()

    # -- lifecycle

    def _start(self) -> None:
        self._stderr = _StderrTail()
        try:
            process = subprocess.Popen(
                self._argv,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.PIPE,
                close_fds=True,
            )
        except OSError as err:
            raise ServerError(f"could not run {self._argv[0]}: {err}") from err
        self._process = process
        self._stderr.follow(process.stderr)
        try:
            self._await_socket()
            if self._wants_http:
                self._await_http()
        except BaseException:
            self._end_process()
            raise

    def _await_socket(self) -> None:
        deadline = time.monotonic() + self._timeout
        while True:
            code = self._process.poll() if self._process else None
            if code is not None:
                # The stderr reader may lag the exit; collect its final
                # diagnostic before building the error.
                self._stderr.stop()
                raise ServerError(self._died(f"exited with status {code}"))
            if _answers(self.socket_path):
                return
            if time.monotonic() >= deadline:
                raise ServerError(self._died(f"did not answer within {self._timeout}s"))
            time.sleep(_POLL)

    def _await_http(self) -> None:
        """Parsed from stderr: the address is known only after binding port 0."""
        deadline = time.monotonic() + self._timeout
        url = self._stderr.wait_for(f"{_PREFIX}http://", deadline)
        token = self._stderr.wait_for(f"{_PREFIX}token: ", deadline)
        if url is None or token is None:
            raise ServerError(self._died("bound no http door"))
        self.http_url = url.removeprefix(_PREFIX).strip()
        self.token = token.removeprefix(f"{_PREFIX}token: ").strip()

    def _died(self, what: str) -> str:
        printed = self._stderr.text().strip()
        detail = f"\n{printed}" if printed else " and printed nothing"
        return f"{BINARY_NAME} {what}:{detail}"

    def _release_client(self) -> None:
        client, self._client = self._client, None
        if client is not None:
            client.close()

    def _end_process(self) -> None:
        process, self._process = self._process, None
        if process is None:
            return
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=_GRACE)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=_GRACE)
        self._stderr.stop()
        if process.stderr is not None:
            process.stderr.close()


def _answers(socket_path: Path) -> bool:
    """Probe on a throwaway connection so a failed probe leaves no state."""
    try:
        with UnixSocketTransport(socket_path, timeout=2.0) as transport:
            return transport.request(SessionList().to_wire()).get("reply") == "ok"
    except (TransportError, OSError):
        return False


def _find_binary(explicit: str | os.PathLike[str] | None) -> str:
    if explicit is not None:
        return _runnable(explicit, "the binary named")
    from_env = os.environ.get(BINARY_ENV)
    if from_env:
        return _runnable(from_env, f"the binary ${BINARY_ENV} names")
    bundled = Path(__file__).resolve().parent / "bin" / BINARY_NAME
    if bundled.exists():
        return _runnable(bundled, "the bundled server")
    on_path = shutil.which(BINARY_NAME)
    if on_path:
        return on_path
    local = Path(__file__).resolve().parents[2] / "target" / "debug" / BINARY_NAME
    if local.is_file():
        return str(local)
    raise ServerError(
        f"no {BINARY_NAME}: install a supported Linux platform wheel, pass "
        f"binary=, set ${BINARY_ENV}, put it on PATH, or run "
        "cargo build -p catchlight-editor-server in the source checkout"
    )


def _runnable(path: str | os.PathLike[str], what: str) -> str:
    resolved = Path(path)
    if not resolved.is_file():
        raise ServerError(f"{what} is not there: {resolved}")
    if not os.access(resolved, os.X_OK):
        raise ServerError(f"{what} is not executable: {resolved}")
    return str(resolved)


class _StderrTail:
    """Drained so a full pipe cannot block the server; kept for launch errors."""

    def __init__(self, limit: int = 400) -> None:
        self._lines: collections.deque[str] = collections.deque(maxlen=limit)
        self._changed = threading.Condition()
        self._thread: threading.Thread | None = None
        self._done = False

    def follow(self, stream: object) -> None:
        if stream is None:
            return
        self._thread = threading.Thread(
            target=self._drain, args=(stream,), name="catchlight-server-stderr", daemon=True
        )
        self._thread.start()

    def _drain(self, stream: object) -> None:
        try:
            for raw in stream:  # type: ignore[attr-defined]
                line = raw.decode("utf-8", "replace").rstrip("\n")
                with self._changed:
                    self._lines.append(line)
                    self._changed.notify_all()
        except (OSError, ValueError):
            pass
        finally:
            with self._changed:
                self._done = True
                self._changed.notify_all()

    def stop(self) -> None:
        if self._thread is not None:
            self._thread.join(timeout=_GRACE)

    def text(self) -> str:
        with self._changed:
            return "\n".join(self._lines)

    def wait_for(self, prefix: str, deadline: float) -> str | None:
        with self._changed:
            while True:
                for line in self._lines:
                    if line.startswith(prefix):
                        return line
                remaining = deadline - time.monotonic()
                if self._done or remaining <= 0:
                    return None
                self._changed.wait(timeout=min(remaining, _POLL))
