"""Transports reconnect before a request, never after one: a mid-request
failure cannot tell whether the command applied."""

from __future__ import annotations

import time
from pathlib import Path

import pytest

from catchlight import (
    Client,
    HttpTransport,
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
    with UnixSocketTransport(server.socket_path, reconnect_after=0.0) as transport:
        client = Client(transport)
        client.send(SessionList())
        client.send(SessionList())
        assert transport.connections == 2


def test_a_closed_connection_is_remade_on_the_next_request(server: LaunchedServer) -> None:
    with UnixSocketTransport(server.socket_path) as transport:
        client = Client(transport)
        session = client.new()
        transport.close()
        client.add_part(session, name="After the reconnect")
        assert transport.connections == 2


def test_a_request_over_the_editors_cap_is_refused_before_it_is_sent(
    server: LaunchedServer,
) -> None:
    """The editor would close the connection; refusing here names the request."""
    client = server.client()
    session = client.new()
    with pytest.raises(TransportError) as raised:
        client.add_part(session, name="x" * MAX_REQUEST_BYTES)
    assert str(MAX_REQUEST_BYTES) in str(raised.value)
    assert client.send(SessionList())


def test_no_editor_at_the_path_is_a_transport_error(tmp_path: Path) -> None:
    with UnixSocketTransport(tmp_path / "nothing.sock") as transport:
        with pytest.raises(TransportError):
            transport.request(SessionList().to_wire())


# -- the http door


def test_a_closed_http_transport_is_remade_on_the_next_request(
    served: LaunchedServer,
) -> None:
    with HttpTransport(served.http_url or "", served.token or "") as transport:
        client = Client(transport)
        session = client.new()
        transport.close()
        client.add_part(session, name="After the reconnect")
        assert client.revision(session) is not None


def test_a_post_over_the_editors_cap_is_refused_before_it_is_sent(
    over_http: Client,
) -> None:
    session = over_http.new()
    with pytest.raises(TransportError) as raised:
        over_http.add_part(session, name="x" * MAX_REQUEST_BYTES)
    assert str(MAX_REQUEST_BYTES) in str(raised.value)
    assert over_http.send(SessionList())


def test_no_editor_at_the_address_is_a_transport_error(served: LaunchedServer) -> None:
    url, token = served.http_url or "", served.token or ""
    served.stop()
    with HttpTransport(url, token) as transport:
        with pytest.raises(TransportError):
            transport.request(SessionList().to_wire())


@pytest.mark.slow
def test_the_socket_survives_a_gap_longer_than_the_editors_idle_timeout(
    server: LaunchedServer,
) -> None:
    """Actually waits out the editor's 30 s idle timeout."""
    client = server.client()
    session = client.new()
    time.sleep(31.0)
    client.add_part(session, name="After the editor hung up")
    assert client.revision(session) is not None
