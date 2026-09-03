"""The HTTP door, against a server serving one.

The socket and the door carry the same commands, so what is worth testing here
is only what differs: the token, the events the socket never pushes, and the
bytes that go over HTTP because the editor's store is not this filesystem.
"""

from __future__ import annotations

import time
from collections.abc import Iterator
from pathlib import Path

import pytest

from catchlight import (
    Client,
    EventDocumentChanged,
    HttpTransport,
    LaunchedServer,
    SessionList,
    TransportError,
    connect,
    launch,
)
from catchlight.protocol_gen import Event, ResponseBodySessions
from support import write_png


@pytest.fixture
def served(tmp_path: Path) -> Iterator[LaunchedServer]:
    """A server with both doors open. Port 0 asks the OS for a free one, and
    the server prints which it got."""
    with launch(store=tmp_path, http="127.0.0.1:0") as running:
        yield running


@pytest.fixture
def over_http(served: LaunchedServer) -> Iterator[Client]:
    with served.connect() as client:
        yield client


def test_the_launch_reports_the_address_and_token_the_server_printed(
    served: LaunchedServer,
) -> None:
    assert served.http_url is not None and served.http_url.startswith("http://127.0.0.1:")
    assert served.token is not None and len(served.token) == 64


def test_a_client_over_the_door_answers_a_query_and_a_document_command(
    over_http: Client,
) -> None:
    body = over_http.send(SessionList())
    assert isinstance(body, ResponseBodySessions)
    assert body.sessions == []

    session = over_http.new()
    part = over_http.add_part(session, name="Body")
    assert part.startswith("root/")
    assert over_http.revision(session) is not None


def test_a_document_command_pushes_an_event_the_socket_never_would(
    served: LaunchedServer, over_http: Client
) -> None:
    """The editor fires observers inside `handle`, so the event may reach the
    connection before the reply it belongs to. Either order queues it."""
    session = over_http.new()
    over_http.add_part(session, name="Body")
    changed = _await_document_changed(over_http, session)
    assert changed.rev == over_http.revision(session)
    assert served.client().events() == [], "the socket pushes none of this"


def test_the_bytes_the_door_hands_out_are_the_bytes_a_save_writes(
    served: LaunchedServer, over_http: Client, tmp_path: Path
) -> None:
    """`save_to` over the door is a fetch and over the socket it is a save; the
    same document has to come out of both."""
    session = over_http.new()
    part = over_http.add_part(session, name="Body")
    over_http.add_texture(session, part, write_png(tmp_path / "body.png"))
    fetched = Path(over_http.save_to(session, tmp_path / "fetched.clm"))

    written = Path(served.client().save(session, str(tmp_path / "written.clm")))
    assert fetched.read_bytes() == written.read_bytes()


def test_a_texture_staged_over_http_is_the_one_the_editor_reads(
    served: LaunchedServer, tmp_path: Path
) -> None:
    """The editor cannot open this script's files over the door, so the bytes
    go up under a key and the command names the key."""
    source = write_png(tmp_path / "body.png")
    transport = HttpTransport(served.http_url or "", served.token or "")
    with Client(transport) as client:
        session = client.new()
        part = client.add_part(session, name="Body")
        texture = client.add_texture(session, part, source)
        assert transport.get_texture(session, texture) == source.read_bytes()


def test_a_wrong_token_never_opens_the_door(served: LaunchedServer) -> None:
    with pytest.raises(TransportError):
        connect(served.http_url or "", "0" * 64)


def test_a_url_that_is_not_loopback_is_refused_without_sending_the_token() -> None:
    with pytest.raises(TransportError) as raised:
        connect("http://example.test:9377", "unused")
    assert "loopback" in str(raised.value)


def _await_document_changed(client: Client, session: int) -> EventDocumentChanged:
    deadline = time.monotonic() + 5.0
    seen: list[Event] = []
    while time.monotonic() < deadline:
        seen += client.events()
        for event in seen:
            if isinstance(event, EventDocumentChanged) and event.session == session:
                return event
        time.sleep(0.02)
    raise AssertionError(f"no document_changed for session {session}, saw {seen}")
