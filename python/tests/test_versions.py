"""Saved versions: numbered snapshots kept beside the model, over both doors."""

from __future__ import annotations

import pytest

from catchlight import Client, ErrorCode, LaunchedServer, ProtocolError, Status
from catchlight.protocol_gen import Kept, ResponseBodyStatus, ReviewIdentity

CLAUDE = ReviewIdentity(id="claude:s1", name="claude")


@pytest.fixture(params=["socket", "http"])
def either(request: pytest.FixtureRequest, served: LaunchedServer) -> Client:
    if request.param == "socket":
        return served.client()
    return served.connect()


def _rev(client: Client, session: int) -> int:
    status = client.send(Status(session=session))
    assert isinstance(status, ResponseBodyStatus)
    return status.status.rev


def test_a_version_keeps_the_model_as_it_was_and_restores_as_one_edit(either: Client) -> None:
    session = either.new()
    first = either.version_save(session, label="baseline", agent=CLAUDE)
    assert (first.version, first.label, first.rev) == (1, "baseline", 0)
    assert _rev(either, session) == 0, "saving a version is no edit"
    baseline = either.export_model(session)
    assert either.version_bytes(session, 1) == baseline

    either.add_part(session, name="ear")
    assert either.version_save(session, agent=None).version == 2
    listed = either.versions(session)
    assert [v.version for v in listed.versions] == [1, 2]
    assert listed.kept is Kept.UNSAVED

    edited = _rev(either, session)
    restored = either.version_restore(session, 1, if_rev=edited)
    assert restored == first
    assert _rev(either, session) == edited + 1
    assert either.export_model(session) == baseline
    either.undo(session)
    assert either.export_model(session) != baseline

    with pytest.raises(ProtocolError) as raised:
        either.version_bytes(session, 9)
    assert raised.value.code is ErrorCode.NO_VERSION


def test_a_version_opens_read_only(either: Client) -> None:
    session = either.new()
    either.version_save(session)
    view = either.version_open(session, 1)
    assert view != session
    assert either.version_open(session, 1) == view
    with pytest.raises(ProtocolError) as raised:
        either.add_part(view, name="ear")
    assert raised.value.code is ErrorCode.READ_ONLY


def test_a_reply_points_at_the_version_it_saved(either: Client) -> None:
    session = either.new()
    thread = either.review_open(session, "the ear tears", agent=None)
    replied = either.review_reply(
        session, thread.thread, "fixed", agent=CLAUDE, save_version="ear fix", key="r1"
    )
    assert replied.comments[-1].version == 1
    again = either.review_reply(
        session, thread.thread, "fixed", agent=CLAUDE, save_version="ear fix", key="r1"
    )
    assert again == replied
    (saved,) = either.versions(session).versions
    assert saved.label == "ear fix"

    pointed = either.review_reply(session, thread.thread, "see v1", agent=CLAUDE, version=1)
    assert pointed.comments[-1].version == 1
    with pytest.raises(ProtocolError) as raised:
        either.review_reply(session, thread.thread, "see v7", agent=CLAUDE, version=7)
    assert raised.value.code is ErrorCode.NO_VERSION
