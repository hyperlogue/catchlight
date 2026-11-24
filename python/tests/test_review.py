"""Review threads travel beside the model; each case runs over both doors."""

from __future__ import annotations

import pytest

from catchlight import Client, ErrorCode, LaunchedServer, ProtocolError, Status
from catchlight.protocol_gen import (
    ResponseBodyStatus,
    ReviewAuthorAgent,
    ReviewDelivery,
    ReviewTargetNode,
    ThreadStatus,
)


@pytest.fixture(params=["socket", "http"])
def either(request: pytest.FixtureRequest, served: LaunchedServer) -> Client:
    if request.param == "socket":
        return served.client()
    return served.connect()


def test_a_comment_reaches_the_agent_only_once_it_is_sent(either: Client) -> None:
    session = either.new()
    opened = either.review_open(session, "the ear tears", agent=None)
    assert [c.delivery for c in opened.comments] == [ReviewDelivery.UNSENT]

    assert either.review_poll(session, agent="claude") == 0
    assert either.review_fetch(session, agent="claude") == []

    either.review_send(session)
    assert either.review_poll(session, agent="claude") == 1
    (taken,) = either.review_fetch(session, agent="claude")
    assert taken.thread == opened.thread
    assert [c.delivery for c in taken.comments] == [ReviewDelivery.SENT]
    assert taken.working == "claude"
    assert either.review_poll(session, agent="claude") == 0

    replied = either.review_reply(session, opened.thread, "fixed", agent="claude")
    assert replied.working is None
    assert replied.comments[-1].author == ReviewAuthorAgent(name="claude")

    review = either.review(session)
    assert [c.delivery for c in review.threads[0].comments] == [
        ReviewDelivery.DELIVERED,
        ReviewDelivery.DELIVERED,
    ]
    assert review.agent is not None and review.agent.name == "claude"


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
