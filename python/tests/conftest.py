"""One editor per test.

A launched server is cheap and a session lives in the process, so a fixture per
test is what keeps one test's sessions out of another's `session_list` — and
what proves, once per test, that a launch cleans up after itself.
"""

from __future__ import annotations

from collections.abc import Iterator
from pathlib import Path

import pytest

from catchlight import Client, LaunchedServer, launch


@pytest.fixture
def server(tmp_path: Path) -> Iterator[LaunchedServer]:
    """An editor of this test's own, whose store is this test's directory."""
    with launch(store=tmp_path) as running:
        yield running


@pytest.fixture
def client(server: LaunchedServer) -> Client:
    return server.client()


@pytest.fixture
def served(tmp_path: Path) -> Iterator[LaunchedServer]:
    """The same editor with its HTTP door open as well.

    Separate from `server` because the door costs a bind and a port, and only
    the tests about it need one. Port 0 asks the OS for a free one, and the
    server prints which it got.
    """
    with launch(store=tmp_path, http="127.0.0.1:0") as running:
        yield running


@pytest.fixture
def over_http(served: LaunchedServer) -> Iterator[Client]:
    """A client onto `served` over the HTTP door rather than the socket."""
    with served.connect() as client:
        yield client
