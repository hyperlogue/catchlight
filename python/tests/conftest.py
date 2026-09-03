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
