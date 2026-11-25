"""One editor per test, so sessions never leak between tests."""

from __future__ import annotations

import os
from collections.abc import Iterator
from pathlib import Path

import pytest

from catchlight import Client, LaunchedServer, launch


@pytest.fixture(autouse=True, scope="session")
def _reviews_kept_apart(tmp_path_factory: pytest.TempPathFactory) -> Iterator[None]:
    """Spawned servers would otherwise keep reviews in the real per-user directory."""
    before = os.environ.get("CATCHLIGHT_REVIEWS")
    os.environ["CATCHLIGHT_REVIEWS"] = str(tmp_path_factory.mktemp("reviews"))
    yield
    if before is None:
        del os.environ["CATCHLIGHT_REVIEWS"]
    else:
        os.environ["CATCHLIGHT_REVIEWS"] = before


@pytest.fixture
def server(tmp_path: Path) -> Iterator[LaunchedServer]:
    with launch(store=tmp_path) as running:
        yield running


@pytest.fixture
def client(server: LaunchedServer) -> Client:
    return server.client()


@pytest.fixture
def served(tmp_path: Path) -> Iterator[LaunchedServer]:
    """Separate from `server` because the HTTP door costs a bind and a port."""
    with launch(store=tmp_path, http="127.0.0.1:0") as running:
        yield running


@pytest.fixture
def over_http(served: LaunchedServer) -> Iterator[Client]:
    with served.connect() as client:
        yield client
