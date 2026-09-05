"""Bytes beside a command, over both doors.

The point of these is that the *wrapper* is the same call whichever transport
is underneath: the socket writes temporary files and names them, HTTP builds a
multipart body, and a caller writes neither. So almost everything here runs
twice, once against `client` (the socket) and once against `over_http`.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from catchlight import (
    Camera,
    Client,
    ErrorCode,
    HttpTransport,
    LaunchedServer,
    NodeTree,
    ProtocolError,
)
from catchlight.protocol_gen import ResponseBodyTextures, ResponseBodyTree, TextureList
from support import png_size, write_manifest, write_png


@pytest.fixture(params=["socket", "http"])
def either(
    request: pytest.FixtureRequest, served: LaunchedServer
) -> Client:
    """The same editor reached both ways, so a test is written once."""
    if request.param == "socket":
        return served.client()
    return served.connect()


def test_a_preview_answers_with_the_png_it_rendered(either: Client) -> None:
    session = either.new()
    png = either.preview(session, size=(48, 32))
    assert png_size(png) == (48, 32)


def test_a_preview_writes_the_file_it_is_asked_for(
    either: Client, tmp_path: Path
) -> None:
    session = either.new()
    out = tmp_path / "shot.png"
    returned = either.preview(session, size=(24, 24), out=out)
    assert out.read_bytes() == returned


def test_a_camera_is_the_commands_and_nobody_elses(
    either: Client, tmp_path: Path
) -> None:
    """Two heights are two pictures, and neither depends on what a tab is
    looking at."""
    session = either.new()
    # A manifest import, so the part has a mesh and there is something to see.
    either.import_manifest(session, write_manifest(tmp_path))
    near = either.preview(
        session, size=(64, 64), camera=Camera(center=(0.0, 0.0), height=48.0)
    )
    far = either.preview(
        session, size=(64, 64), camera=Camera(center=(0.0, 0.0), height=512.0)
    )
    assert near != far


def test_a_model_imports_into_a_fresh_session(
    either: Client, served: LaunchedServer, tmp_path: Path
) -> None:
    """The bytes are read here and travel with the command, so this works over
    a door the editor cannot read files through."""
    authored = either.new()
    either.add_part(authored, name="Body")
    source = Path(served.client().save(authored, str(tmp_path / "authored.clm")))

    into = either.new()
    either.import_file(into, source)
    body = either.send(NodeTree(session=into))
    assert isinstance(body, ResponseBodyTree)
    assert [child.name for child in body.root.children] == ["Body"]


def test_a_second_import_into_the_same_session_is_refused(
    either: Client, served: LaunchedServer, tmp_path: Path
) -> None:
    authored = either.new()
    either.add_part(authored, name="Body")
    source = Path(served.client().save(authored, str(tmp_path / "authored.clm")))

    into = either.new()
    either.import_file(into, source)
    with pytest.raises(ProtocolError) as raised:
        either.import_file(into, source)
    assert raised.value.code is ErrorCode.NOT_EMPTY


def test_a_manifest_and_its_images_travel_with_the_command(
    either: Client, tmp_path: Path
) -> None:
    """The texture reference has a separator in it, and the attachment name
    carries it verbatim."""
    manifest = write_manifest(tmp_path)
    session = either.new()
    either.import_manifest(session, manifest)

    body = either.send(NodeTree(session=session))
    assert isinstance(body, ResponseBodyTree)
    assert len(body.root.children) == 1

    # The image arrived: an import whose reference went unattached is refused
    # outright, and the texture it built is in the model.
    textures = either.send(TextureList(session=session))
    assert isinstance(textures, ResponseBodyTextures)
    assert len(textures.textures) == 1


def test_a_texture_travels_with_the_command_over_either_door(
    either: Client, served: LaunchedServer, tmp_path: Path
) -> None:
    """Nothing is staged first, so a `texture_add` is one round trip on both
    transports and the editor holds exactly the bytes that were sent."""
    source = write_png(tmp_path / "body.png")
    session = either.new()
    part = either.add_part(session, name="Body")
    texture = either.add_texture(session, part, source)

    # Read back through the door that hands out bytes, whichever door the
    # command went in through.
    reader = HttpTransport(served.http_url or "", served.token or "")
    with Client(reader):
        assert reader.get_texture(session, texture) == source.read_bytes()
