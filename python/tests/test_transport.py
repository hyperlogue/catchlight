"""The socket transport's one hard rule: reconnect before a request, never
after one.

The editor closes a connection idle for 30 s. A client that discovered that
half way through a `node_add` could not tell whether the node was added, so the
transport remakes a connection at 20 s and lets a mid-request failure raise.
"""

from __future__ import annotations

import time
from pathlib import Path

import pytest

from catchlight import (
    Client,
    LaunchedServer,
    SessionList,
    TransportError,
    UnixSocketTransport,
)
from catchlight.transport import MAX_REQUEST_BYTES


def test_a_fresh_connection_is_reused_across_requests(server: LaunchedServer) -> None:
    with UnixSocketTransport(server.socket_path) as transport:
        client = Client(transport)
        client.send(SessionList())
        client.send(SessionList())
        assert transport.connections == 1


def test_a_connection_past_the_window_is_remade_before_the_request(
    server: LaunchedServer,
) -> None:
    """A window of zero makes every request the one after a long gap."""
    with UnixSocketTransport(server.socket_path, reconnect_after=0.0) as transport:
        client = Client(transport)
        session = client.new()
        client.add_part(session, name="Body")
        assert transport.connections == 2


def test_a_closed_connection_is_remade_on_the_next_request(server: LaunchedServer) -> None:
    """Sessions are the editor's, not the connection's, so a client survives
    losing one."""
    with UnixSocketTransport(server.socket_path) as transport:
        client = Client(transport)
        session = client.new()
        transport.close()
        client.add_part(session, name="After the reconnect")
        assert transport.connections == 2


def test_a_request_over_the_editors_cap_is_refused_before_it_is_sent(
    server: LaunchedServer,
) -> None:
    """The editor answers a line this long by closing the connection, so the
    transport stops it here, where the error still names the request."""
    client = server.client()
    session = client.new()
    with pytest.raises(TransportError) as raised:
        client.add_part(session, name="x" * MAX_REQUEST_BYTES)
    assert str(MAX_REQUEST_BYTES) in str(raised.value)
    # Nothing reached the wire, so the next request is fine.
    assert client.send(SessionList())


def test_no_editor_at_the_path_is_a_transport_error(tmp_path: Path) -> None:
    with UnixSocketTransport(tmp_path / "nothing.sock") as transport:
        with pytest.raises(TransportError):
            transport.request(SessionList().to_wire())


@pytest.mark.slow
def test_the_socket_survives_a_gap_longer_than_the_editors_idle_timeout(
    server: LaunchedServer,
) -> None:
    """The one test that actually waits out `SOCKET_IO_TIMEOUT` (30 s).

    Everything else about the reconnect is checked by shrinking the window,
    which proves the rule holds and not that the rule is the right one.
    """
    client = server.client()
    session = client.new()
    time.sleep(31.0)
    client.add_part(session, name="After the editor hung up")
    assert client.revision(session) is not None
