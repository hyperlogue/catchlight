"""The HTTP door, against a server serving one.

The socket and the door carry the same commands, so what is worth testing here
is only what differs: the token, the loopback rule, and the bytes that go over
HTTP because the editor's store is not this filesystem. How the connection
under those is made and remade is `test_transport.py`.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from catchlight import (
    Client,
    HttpTransport,
    LaunchedServer,
    SessionList,
    TransportError,
    connect,
)
from catchlight.protocol_gen import ResponseBodySessions
from support import write_png


def test_the_launch_reports_the_address_and_token_the_server_printed(
    served: LaunchedServer,
) -> None:
    assert served.http_url is not None and served.http_url.startswith("http://127.0.0.1:")
    assert served.token is not None and len(served.token) == 64


def test_a_client_over_the_door_answers_a_query_and_an_edit(
    over_http: Client,
) -> None:
    body = over_http.send(SessionList())
    assert isinstance(body, ResponseBodySessions)
    assert body.sessions == []

    session = over_http.new()
    part = over_http.add_part(session, name="Body")
    assert part.startswith("root/")
    assert over_http.revision(session) is not None


def test_an_edit_over_the_door_is_the_editors_and_not_the_connections(
    served: LaunchedServer, over_http: Client
) -> None:
    """Every POST is its own round trip, so what an edit changed has to outlive
    the connection that carried it — and be there for a client on the other
    door, which is how a script and a tab share one editor."""
    session = over_http.new()
    over_http.add_part(session, name="Body")
    rev = over_http.revision(session)

    body = served.client().send(SessionList())
    assert isinstance(body, ResponseBodySessions)
    listed = [info for info in body.sessions if info.session == session]
    assert [info.rev for info in listed] == [rev]


def test_the_bytes_the_door_hands_out_are_the_bytes_a_save_writes(
    served: LaunchedServer, over_http: Client, tmp_path: Path
) -> None:
    """`save_to` over the door is a fetch and over the socket it is a save; the
    same model has to come out of both."""
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


def test_a_wrong_token_is_refused_on_the_first_command(served: LaunchedServer) -> None:
    """A command is one POST, so there is no connection for `connect` to open
    and nothing for it to refuse; the token is checked on the first request."""
    client = connect(served.http_url or "", "0" * 64)
    with pytest.raises(TransportError) as raised:
        client.send(SessionList())
    assert "401" in str(raised.value)


def test_a_url_that_is_not_loopback_is_refused_without_sending_the_token() -> None:
    with pytest.raises(TransportError) as raised:
        connect("http://example.test:9377", "unused")
    assert "loopback" in str(raised.value)
