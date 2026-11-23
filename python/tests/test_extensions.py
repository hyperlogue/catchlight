"""Extensions are carried, never interpreted; each case runs over both doors."""

from __future__ import annotations

import pytest

from catchlight import Client, ErrorCode, LaunchedServer, ProtocolError
from catchlight.protocol_gen import ExtensionValueInfoBytes, ExtensionValueInfoJson


@pytest.fixture(params=["socket", "http"])
def either(request: pytest.FixtureRequest, served: LaunchedServer) -> Client:
    if request.param == "socket":
        return served.client()
    return served.connect()


def test_a_json_value_comes_back_as_what_went_in(either: Client) -> None:
    session = either.new()
    value = {"palette": ["#fff", "#000"], "version": 2, "nested": [1, 2, {"a": None}]}
    either.extension_set(session, "acme.camera", value)

    assert either.extension_get(session, "acme.camera") == value


def test_bytes_travel_attached_and_come_back_as_the_payload(either: Client) -> None:
    session = either.new()
    blob = bytes(range(256)) * 4

    either.extension_set(session, "acme.thumb", blob)

    assert either.extension_get(session, "acme.thumb") == blob


def test_the_listing_reports_a_marker_rather_than_the_bytes(either: Client) -> None:
    session = either.new()
    either.extension_set(session, "acme.thumb", b"a thumbnail")
    either.extension_set(session, "acme.camera", {"v": 1})

    listed = either.extensions(session)
    assert [e.key for e in listed] == ["acme.camera", "acme.thumb"]

    json_value = listed[0].value
    assert isinstance(json_value, ExtensionValueInfoJson)
    assert json_value.value == {"v": 1}

    marker = listed[1].value
    assert isinstance(marker, ExtensionValueInfoBytes)
    assert marker.size == len(b"a thumbnail")
    assert len(marker.hash) == 64


def test_a_delete_takes_the_key_with_it(either: Client) -> None:
    session = either.new()
    either.extension_set(session, "acme.camera", {"v": 1})
    either.extension_delete(session, "acme.camera")

    assert either.extensions(session) == []
    with pytest.raises(ProtocolError) as raised:
        either.extension_get(session, "acme.camera")
    assert raised.value.code is ErrorCode.NO_EXTENSION


def test_the_formats_own_prefix_is_refused(either: Client) -> None:
    session = either.new()
    with pytest.raises(ProtocolError) as raised:
        either.extension_set(session, "catchlight.thumb", {"v": 1})
    assert raised.value.code is ErrorCode.RESERVED_EXTENSION


def test_an_extension_survives_a_save_and_an_open(
    either: Client, served: LaunchedServer, tmp_path
) -> None:
    session = either.new()
    either.extension_set(session, "acme.camera", {"rig": "v2"})
    either.extension_set(session, "acme.thumb", b"a thumbnail")
    saved = either.save_to(session, tmp_path / "rig.clm")

    reopened = served.client().open(saved)
    reader = served.client()
    assert reader.extension_get(reopened, "acme.camera") == {"rig": "v2"}
    assert reader.extension_get(reopened, "acme.thumb") == b"a thumbnail"
