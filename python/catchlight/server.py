"""Starting an editor, and attaching to one somebody else started.

[`launch`] runs `catchlight-editor-server` on a socket of its own and hands
back a [`LaunchedServer`] that owns the process; [`connect`] opens a
[`Client`](catchlight.client.Client) onto a server already running and owns
nothing. Both end at a client — this module is only about who is responsible
for the process behind it.

Invariants this module enforces:

- **A launched server is private.** The socket lives in a fresh `0700` temp
  directory, which is the whole of its access control: nothing else on the
  machine can reach into the directory, so nothing else can reach the socket,
  and there is no token to check. It also means a launched server never
  collides with the editor a person is already running on the canonical
  socket.

- **Nothing is left behind.** The process is terminated, given a grace period,
  killed if it has not gone, and the temp directory removed — on a clean exit,
  on an exception, and on a launch that never came up at all.

- **Ready means answered.** The launch waits until a `session_list` over the
  socket comes back, not until the file appears and not for a line on stderr.
  A server that exits first fails the launch immediately, with what it printed.

- **Sessions live in the process, not the connection.** So [`restart`] loses
  every open session, and a client held across one is talking to a new editor
  with an empty session list.

- **[`connect`] never stops anything.** It did not start the server, and a
  script that attaches to the editor a person has open must not take it down
  when it finishes.

Unix-only, because the socket is: a Windows launch waits for an HTTP-only
`main` in the server itself.
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

# The binary this package drives, and the environment variable that overrides
# where it is found.
BINARY_NAME = "catchlight-editor-server"
BINARY_ENV = "CATCHLIGHT_EDITOR_SERVER"

# Every line the server prints starts with its own name.
_PREFIX = f"{BINARY_NAME}: "
# How often the launch asks whether the socket answers yet.
_POLL = 0.05
# How long a terminated server has to exit before it is killed.
_GRACE = 3.0


class ServerError(RuntimeError):
    """A server could not be started, or did not come up, or is not there."""


def launch(
    *,
    binary: str | os.PathLike[str] | None = None,
    store: str | os.PathLike[str] | None = None,
    model: str | os.PathLike[str] | None = None,
    http: str | None = None,
    timeout: float = 10.0,
) -> LaunchedServer:
    """Start an editor on a private socket and wait until it answers.

    `binary` is the server to run; absent, it is `$CATCHLIGHT_EDITOR_SERVER`,
    then `catchlight-editor-server` on `PATH`, then this workspace's own
    `target/debug` build — the last of those so the tests in this repository
    need no setup beyond `cargo build`.

    `store` is what a relative storage key resolves against, the current
    directory unless it is named. The client sends absolute paths, so the root
    matters only to a raw `send` that does not.

    `model` is a `.clm` to open into a first session.

    `http` is an address like `127.0.0.1:0` to serve the browser door on beside
    the socket, for a client that wants [`LaunchedServer.connect`]; port 0 asks
    the OS for a free one and [`LaunchedServer.http_url`] says which.

    Use it as a context manager — `with catchlight.launch() as server:` — so
    the process and its directory go away however the block ends.
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
    """A client onto a server something else is running, over its HTTP door.

    The caller owns the connection and nothing else: closing this client leaves
    the editor exactly as it found it. Nothing is sent here either — a command
    is one POST, so a wrong token or an editor that has gone away surfaces on
    the first `send` rather than on this call.
    """
    return Client(HttpTransport(url, token))


class LaunchedServer:
    """A running editor and the temp directory its socket lives in."""

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
        """The server's process id, or `None` once it has been stopped."""
        return None if self._process is None else self._process.pid

    def client(self) -> Client:
        """The client onto this server, made on first use and reused after.

        One client per server so its revisions are one running account of what
        this script has done, rather than one per call.
        """
        if self._process is None:
            raise ServerError("this server has been stopped")
        if self._client is None:
            self._client = Client(UnixSocketTransport(self.socket_path))
        return self._client

    def connect(self) -> Client:
        """A second client onto the same editor, over its HTTP door.

        Only when [`launch`] was given an `http` address; a socket-only server
        has no door to connect to. Like [`connect`], it sends nothing until the
        first command.
        """
        if self.http_url is None or self.token is None:
            raise ServerError("this server was launched without an http address")
        return Client(HttpTransport(self.http_url, self.token))

    def healthy(self) -> bool:
        """Whether a `session_list` comes back."""
        if self._process is None or self._process.poll() is not None:
            return False
        try:
            self.client().send(SessionList())
        except (ProtocolError, ServerError, TransportError):
            return False
        return True

    def stderr(self) -> str:
        """What the server has printed, most recent lines last."""
        return self._stderr.text()

    def stop(self) -> None:
        """Terminate the server, kill it if it lingers, and remove its
        directory. Calling it twice is not an error."""
        self._release_client()
        self._end_process()
        shutil.rmtree(self.directory, ignore_errors=True)

    def restart(self) -> None:
        """Stop the process and start another on the same socket.

        The new editor holds no sessions: they lived in the process that just
        went away.
        """
        self._release_client()
        self._end_process()
        self._start()

    # -- lifecycle

    def _start(self) -> None:
        self._stderr = _StderrTail()
        try:
            # argv is built in `launch`, never a string and never a shell.
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
                raise ServerError(self._died(f"exited with status {code}"))
            if _answers(self.socket_path):
                return
            if time.monotonic() >= deadline:
                raise ServerError(self._died(f"did not answer within {self._timeout}s"))
            time.sleep(_POLL)

    def _await_http(self) -> None:
        """The address and token the server printed once it bound the door.

        Parsed from stderr because that is where the server says it: the
        address is only known after a `--http` port of 0 is resolved.
        """
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
    """Whether a `session_list` over the socket comes back.

    A connection of its own, thrown away after: this runs before the client
    exists, and a probe that failed must leave nothing for the client to
    inherit.
    """
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
    on_path = shutil.which(BINARY_NAME)
    if on_path:
        return on_path
    # This workspace's own debug build, so its tests need nothing on PATH
    # beyond what `cargo build -p catchlight-editor-server` already made.
    local = Path(__file__).resolve().parents[2] / "target" / "debug" / BINARY_NAME
    if local.is_file():
        return str(local)
    raise ServerError(
        f"no {BINARY_NAME}: pass binary=, set ${BINARY_ENV}, put it on PATH, "
        f"or build it into {local}"
    )


def _runnable(path: str | os.PathLike[str], what: str) -> str:
    resolved = Path(path)
    if not resolved.is_file():
        raise ServerError(f"{what} is not there: {resolved}")
    return str(resolved)


class _StderrTail:
    """The server's stderr, drained by a thread and kept as its last lines.

    Drained rather than left in the pipe because a server nobody reads blocks
    once the pipe buffer fills, and kept rather than forwarded because the one
    thing a failed launch owes its caller is what the server said on the way
    down.
    """

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
        """The first line starting with `prefix`, waiting for it until
        `deadline`. Lines already read count."""
        with self._changed:
            while True:
                for line in self._lines:
                    if line.startswith(prefix):
                        return line
                remaining = deadline - time.monotonic()
                if self._done or remaining <= 0:
                    return None
                self._changed.wait(timeout=min(remaining, _POLL))
