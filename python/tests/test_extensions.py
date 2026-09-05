"""Extensions from Python: carried, never interpreted.

The kind is the Python type — `bytes` in, `bytes` out; anything else is JSON —
and the transport under it is not the caller's business, so every case here
runs twice, once over the socket and once over HTTP.
"""

from __future__ import annotations

import pytest

from catchlight import Client, ErrorCode, LaunchedServer, ProtocolError
from catchlight.protocol_gen import ExtensionValueInfoBytes, ExtensionValueInfoJson


@pytest.fixture(params=["socket", "http"])
def either(request: pytest.FixtureRequest, served: LaunchedServer) -> Client:
    """The same editor reached both ways, so a test is written once."""
    if request.param == "socket":
        return served.client()
    return served.connect()


def test_a_json_value_comes_back_as_what_went_in(either: Client) -> None:
    session = either.new()
    value = {"palette": ["#fff", "#000"], "version": 2, "nested": [1, 2, {"a": None}]}
    either.extension_set(session, "molan.caster", value)

    assert either.extension_get(session, "molan.caster") == value


def test_bytes_travel_attached_and_come_back_as_the_payload(either: Client) -> None:
    session = either.new()
    blob = bytes(range(256)) * 4

    either.extension_set(session, "molan.thumb", blob)

    assert either.extension_get(session, "molan.thumb") == blob


def test_the_listing_reports_a_marker_rather_than_the_bytes(either: Client) -> None:
    session = either.new()
    either.extension_set(session, "molan.thumb", b"a thumbnail")
    either.extension_set(session, "molan.caster", {"v": 1})

    listed = either.extensions(session)
    assert [e.key for e in listed] == ["molan.caster", "molan.thumb"]

    json_value = listed[0].value
    assert isinstance(json_value, ExtensionValueInfoJson)
    assert json_value.value == {"v": 1}

    marker = listed[1].value
    assert isinstance(marker, ExtensionValueInfoBytes)
    assert marker.size == len(b"a thumbnail")
    # The hash is what a feed compares; the bytes are not in the listing.
    assert len(marker.hash) == 64


def test_a_delete_takes_the_key_with_it(either: Client) -> None:
    session = either.new()
    either.extension_set(session, "molan.caster", {"v": 1})
    either.extension_delete(session, "molan.caster")

    assert either.extensions(session) == []
    with pytest.raises(ProtocolError) as raised:
        either.extension_get(session, "molan.caster")
    assert raised.value.code is ErrorCode.NO_EXTENSION


def test_the_formats_own_prefix_is_refused(either: Client) -> None:
    session = either.new()
    with pytest.raises(ProtocolError) as raised:
        either.extension_set(session, "catchlight.thumb", {"v": 1})
    assert raised.value.code is ErrorCode.RESERVED_EXTENSION


def test_an_extension_survives_a_save_and_an_open(
    either: Client, served: LaunchedServer, tmp_path
) -> None:
    """The whole point: a vendor's value is carried across a round trip through
    the format, byte for byte."""
    session = either.new()
    either.extension_set(session, "molan.caster", {"rig": "v2"})
    either.extension_set(session, "molan.thumb", b"a thumbnail")
    saved = either.save_to(session, tmp_path / "rig.clm")

    reopened = served.client().open(saved)
    reader = served.client()
    assert reader.extension_get(reopened, "molan.caster") == {"rig": "v2"}
    assert reader.extension_get(reopened, "molan.thumb") == b"a thumbnail"
