"""What `launch` owes its caller: a server that answers, and no leftovers.

Every assertion about cleanup is made from outside the launch — the process id
after the process is gone, the temp directories in `$TMPDIR` — because a launch
that failed halfway has no object left to ask.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from catchlight import Client, LaunchedServer, ServerError, SessionList, launch
from catchlight.protocol_gen import ResponseBodySessions
from support import is_running, launched_directories


def test_a_launch_answers_and_exits_leaving_nothing_behind(tmp_path: Path) -> None:
    before = launched_directories()
    running = launch(store=tmp_path)
    pid = running.pid
    assert pid is not None
    assert running.healthy()
    assert running.socket_path.exists()
    assert launched_directories() - before == {running.directory}

    running.stop()
    assert running.pid is None
    assert not is_running(pid)
    assert not running.directory.exists()
    assert launched_directories() - before == set()


def test_stopping_twice_is_not_an_error(tmp_path: Path) -> None:
    running = launch(store=tmp_path)
    running.stop()
    running.stop()
    assert not running.healthy()


def test_the_context_manager_stops_the_server_when_the_block_raises(tmp_path: Path) -> None:
    seen: list[tuple[int, Path]] = []
    with pytest.raises(ZeroDivisionError):
        with launch(store=tmp_path) as running:
            assert running.pid is not None
            seen.append((running.pid, running.directory))
            raise ZeroDivisionError
    (pid, directory) = seen[0]
    assert not is_running(pid)
    assert not directory.exists()


def test_a_binary_that_is_not_there_raises_and_leaves_nothing(tmp_path: Path) -> None:
    before = launched_directories()
    with pytest.raises(ServerError) as raised:
        launch(binary=tmp_path / "not-a-server", store=tmp_path)
    assert "not-a-server" in str(raised.value)
    assert launched_directories() == before


def test_a_model_that_is_not_there_raises_with_what_the_server_printed(tmp_path: Path) -> None:
    before = launched_directories()
    missing = tmp_path / "missing.clm"
    with pytest.raises(ServerError) as raised:
        launch(store=tmp_path, model=missing)
    message = str(raised.value)
    assert "could not open" in message, message
    assert str(missing) in message, message
    assert launched_directories() == before


def test_a_launch_opens_the_model_it_is_given(tmp_path: Path, client: Client) -> None:
    """The saved file is opened by the Rust reader, which is the only
    validation this client has."""
    session = client.new()
    saved = client.save_to(session, tmp_path / "opened.clm")
    with launch(store=tmp_path, model=saved) as reopened:
        body = reopened.client().send(SessionList())
        assert isinstance(body, ResponseBodySessions)
        assert [info.file for info in body.sessions] == [saved]


def test_restart_gives_a_working_server_that_holds_no_sessions(tmp_path: Path) -> None:
    with launch(store=tmp_path) as running:
        first = running.pid
        running.client().new()
        assert _session_count(running) == 1

        running.restart()
        assert running.pid != first
        assert running.healthy()
        assert _session_count(running) == 0
        assert first is not None and not is_running(first)


def test_a_stopped_server_hands_out_no_client(tmp_path: Path) -> None:
    running = launch(store=tmp_path)
    running.stop()
    with pytest.raises(ServerError):
        running.client()


def _session_count(running: LaunchedServer) -> int:
    body = running.client().send(SessionList())
    assert isinstance(body, ResponseBodySessions)
    return len(body.sessions)
