"""Review threads travel beside the model; each case runs over both doors."""

from __future__ import annotations

from pathlib import Path

import pytest

from catchlight import Client, ErrorCode, LaunchedServer, ProtocolError, Status
from catchlight.protocol_gen import (
    ResponseBodyStatus,
    ReviewAuthorAgent,
    ReviewDelivery,
    ReviewHeld,
    ReviewIdentity,
    ReviewTargetNode,
    ThreadStatus,
)

CLAUDE = ReviewIdentity(id="claude:s1", name="claude")
CODEX = ReviewIdentity(id="codex:t1", name="codex")


@pytest.fixture(params=["socket", "http"])
def either(request: pytest.FixtureRequest, served: LaunchedServer) -> Client:
    if request.param == "socket":
        return served.client()
    return served.connect()


def test_a_comment_reaches_the_agent_only_once_it_is_sent(either: Client) -> None:
    session = either.new()
    opened = either.review_open(session, "the ear tears", agent=None)
    assert [c.delivery for c in opened.comments] == [ReviewDelivery.UNSENT]

    assert either.review_poll(session, agent=CLAUDE) == 0
    nothing = either.review_fetch(session, agent=CLAUDE)
    assert (nothing.threads, nothing.batch) == ([], None)

    either.review_send(session)
    assert either.review_poll(session, agent=CLAUDE) == 1
    fetched = either.review_fetch(session, agent=CLAUDE)
    assert either.review_fetch(session, agent=CLAUDE) == fetched, "nothing taken yet"
    (taken,) = fetched.threads
    assert taken.thread == opened.thread
    assert [c.delivery for c in taken.comments] == [ReviewDelivery.SENT]
    assert fetched.batch is not None

    acked = either.review_ack(session, fetched.batch, agent=CLAUDE)
    assert (acked.claimed, acked.held) == ([opened.thread], [])
    assert either.review_poll(session, agent=CLAUDE) == 0
    with pytest.raises(ProtocolError) as raised:
        either.review_ack(session, fetched.batch, agent=CODEX)
    assert raised.value.code is ErrorCode.BATCH_STALE

    replied = either.review_reply(session, opened.thread, "fixed", agent=CLAUDE)
    assert replied.holder is None
    assert replied.comments[-1].author == ReviewAuthorAgent(id="claude:s1", name="claude")

    review = either.review(session)
    assert [c.delivery for c in review.threads[0].comments] == [
        ReviewDelivery.DELIVERED,
        ReviewDelivery.DELIVERED,
    ]
    assert [a.name for a in review.agents] == ["claude"], "a refused command is not heard"


def test_a_claim_is_one_agents_until_released(either: Client) -> None:
    session = either.new()
    opened = either.review_open(session, "which ear?", agent=CODEX)
    assert either.review_claim(session, [opened.thread], agent=CLAUDE) == 3600
    assert either.review(session).threads[0].holder == CLAUDE
    with pytest.raises(ProtocolError) as raised:
        either.review_claim(session, [opened.thread], agent=CODEX)
    assert raised.value.code is ErrorCode.THREAD_CLAIMED
    assert "claude:s1" in raised.value.message

    either.review_release(session, [opened.thread], agent=CLAUDE)
    assert either.review_claim(session, [opened.thread], agent=CODEX) == 3600

    either.review_reply(session, opened.thread, "the left", agent=None)
    either.review_send(session)
    fetched = either.review_fetch(session, agent=CLAUDE)
    assert fetched.threads == [], "codex holds it"
    fetched = either.review_fetch(session, agent=CODEX)
    assert fetched.batch is not None
    either.review_release(session, [opened.thread], agent=CODEX)
    either.review_claim(session, [opened.thread], agent=CLAUDE)
    acked = either.review_ack(session, fetched.batch, agent=CODEX)
    assert acked.held == [ReviewHeld(thread=opened.thread, holder=CLAUDE)]


def test_a_retried_write_lands_once(either: Client) -> None:
    session = either.new()
    first = either.review_open(session, "a question", agent=CLAUDE, key="q1")
    again = either.review_open(session, "a question", agent=CLAUDE, key="q1")
    assert again == first
    replied = either.review_reply(session, first.thread, "more", agent=CLAUDE, key="r1")
    assert either.review_reply(session, first.thread, "more", agent=CLAUDE, key="r1") == replied
    assert len(either.review(session).threads[0].comments) == 2

    with pytest.raises(ProtocolError) as raised:
        either.review_reply(session, first.thread, "other", agent=CLAUDE, key="r1")
    assert raised.value.code is ErrorCode.KEY_CONFLICT


def test_a_review_moves_no_revision(either: Client) -> None:
    session = either.new()
    either.review_open(session, "a question")
    resolved = either.resolve(session, 1)

    assert resolved.status is ThreadStatus.RESOLVED
    assert either.resolve(session, 1, resolved=False).status is ThreadStatus.OPEN
    status = either.send(Status(session=session))
    assert isinstance(status, ResponseBodyStatus)
    assert status.status.rev == 0
    assert not status.status.dirty


def test_a_thread_needs_a_target_the_model_holds(either: Client) -> None:
    session = either.new()
    with pytest.raises(ProtocolError) as raised:
        either.review_open(session, "where?", target=ReviewTargetNode(node="nope"))
    assert raised.value.code is ErrorCode.NO_NODE

    with pytest.raises(ProtocolError) as raised:
        either.review_reply(session, 4, "to nothing")
    assert raised.value.code is ErrorCode.NO_THREAD


PNG = b"\x89PNG\r\n\x1a\n-not-really-a-png"


def test_an_image_rides_on_a_comment_and_reads_back(either: Client, tmp_path: Path) -> None:
    session = either.new()
    shot = tmp_path / "shot.png"
    shot.write_bytes(PNG)

    opened = either.review_open(session, agent=None, attach=[shot])
    (image,) = opened.comments[0].images
    assert opened.comments[0].body == ""
    assert (image.media_type, image.byte_length) == ("image/png", len(PNG))
    assert either.review_image(session, image.image) == PNG

    jpeg = b"\xff\xd8\xff\xe0 after"
    replied = either.review_reply(session, opened.thread, "like this", attach=[jpeg])
    (answer,) = replied.comments[-1].images
    assert answer.media_type == "image/jpeg"
    assert either.review_image(session, answer.image) == jpeg


def test_an_image_is_refused_by_its_bytes_and_a_comment_by_its_images(either: Client) -> None:
    session = either.new()
    with pytest.raises(ProtocolError) as raised:
        either.review_open(session, "a gif", attach=[b"GIF89a"])
    assert raised.value.code is ErrorCode.BAD_REQUEST

    with pytest.raises(ProtocolError) as raised:
        either.review_open(session, "too many", attach=[PNG] * 5)
    assert raised.value.code is ErrorCode.LIMIT_EXCEEDED
    assert raised.value.limit is not None
    assert raised.value.limit.resource == "review_comment_images"

    with pytest.raises(ProtocolError) as raised:
        either.review_image(session, 99)
    assert raised.value.code is ErrorCode.NO_IMAGE
    assert either.review(session).threads == []
