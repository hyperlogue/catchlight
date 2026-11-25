"""The official extensions' typed commands, over both doors."""

from __future__ import annotations

import pytest

from catchlight import Client, ErrorCode, LaunchedServer, ProtocolError
from catchlight.protocol_gen import (
    ClipInfo,
    ControlMappingInfo,
    ExpressionGroup,
    ExpressionSetting,
    ExtensionRefParam,
    LaneInfo,
    LaneTargetInfoControl,
    LaneTargetInfoParam,
    ParamAdd,
    ResponseBodyClips,
    ResponseBodyParam,
    parse_response_body,
)


@pytest.fixture(params=["socket", "http"])
def either(request: pytest.FixtureRequest, served: LaunchedServer) -> Client:
    if request.param == "socket":
        return served.client()
    return served.connect()


def rig(client: Client) -> tuple[int, str]:
    session = client.new()
    body = client.send(ParamAdd(session=session, name="jaw", min=0.0, max=1.0, default=0.2))
    assert isinstance(body, ResponseBodyParam)
    return session, body.param


def test_mapping_a_control_reads_back_and_unmapping_takes_it_away(either: Client) -> None:
    session, jaw = rig(either)
    mapped = either.humanoid_map(session, "mouth_open", jaw)
    assert mapped.mapping == ControlMappingInfo(
        control="mouth_open", param=jaw, stops=(0.2, 0.2, 1.0)
    )
    assert mapped.replaced is None

    listed = either.humanoid(session)
    assert listed.mappings == [mapped.mapping]
    assert any(c.name == "mouth_open" and c.one_sided for c in listed.controls)

    assert either.humanoid_unmap(session, "mouth_open") == mapped.mapping
    assert either.humanoid(session).mappings == []


def test_a_nonstandard_control_is_refused_unless_allowed(either: Client) -> None:
    session, jaw = rig(either)
    with pytest.raises(ProtocolError) as raised:
        either.humanoid_map(session, "mouth_opn", jaw)
    assert raised.value.code is ErrorCode.NONSTANDARD_CONTROL
    assert "mouth_open" in str(raised.value)
    either.humanoid_map(session, "tail_wag", jaw, stops=(0.0, 0.5, 1.0), nonstandard=True)
    assert either.humanoid(session).mappings[0].stops == (0.0, 0.5, 1.0)


def test_expressions_and_visemes_are_set_listed_and_deleted(either: Client) -> None:
    session, jaw = rig(either)
    sad = [
        ExpressionSetting(param=jaw, value=0.5),
        ExpressionSetting(control="mouth_smile", value=-0.5),
    ]
    stored = either.expression_set(session, "sad", sad)
    assert stored.expression.settings == sad
    either.expression_set(
        session,
        "aa",
        [ExpressionSetting(control="mouth_open", value=1.0)],
        group=ExpressionGroup.VISEME,
    )
    listed = either.expressions(session)
    assert [e.name for e in listed.expressions] == ["sad"]
    assert [v.name for v in listed.visemes] == ["aa"]
    assert "happy" in listed.emotion_names

    added = either.viseme_add_standard(session)
    assert added.skipped == ["aa"]
    assert len(added.added) == len(listed.viseme_names) - 1

    removed = either.expression_delete(session, "sad")
    assert removed.name == "sad"
    with pytest.raises(ProtocolError) as raised:
        either.expression_delete(session, "sad")
    assert raised.value.code is ErrorCode.EDIT


def test_a_setting_out_of_its_controls_range_is_refused(either: Client) -> None:
    session, _ = rig(either)
    with pytest.raises(ProtocolError) as raised:
        either.expression_set(session, "x", [ExpressionSetting(control="mouth_open", value=-1.0)])
    assert raised.value.code is ErrorCode.BAD_REQUEST


def test_extension_warnings_name_their_subjects(either: Client) -> None:
    session, jaw = rig(either)
    either.humanoid_map(session, "head_yaw", jaw)
    either.humanoid_map(session, "head_pitch", jaw)
    warnings = either.extension_warnings(session)
    shared = [w for w in warnings if w.code == "catchlight.humanoid/shared-param"]
    assert len(shared) == 1
    assert shared[0].key == "catchlight.humanoid"
    assert shared[0].subjects == [ExtensionRefParam(param=jaw)]


def test_a_model_without_clips_lists_none(either: Client) -> None:
    session, _ = rig(either)
    assert either.clips(session) == []


def test_a_clip_listing_decodes_each_lane_target() -> None:
    body = parse_response_body(
        {
            "result": "clips",
            "clips": [
                {
                    "name": "talk",
                    "timestep": 0.1,
                    "length": 12,
                    "lead_in": 2,
                    "lead_out": None,
                    "lanes": [
                        {"target": {"kind": "param", "param": "jaw"}, "keyframes": 2},
                        {"target": {"kind": "control", "name": "mouth_open"}, "keyframes": 1},
                    ],
                }
            ],
        }
    )
    assert body == ResponseBodyClips(
        clips=[
            ClipInfo(
                name="talk",
                timestep=0.1,
                length=12,
                lead_in=2,
                lead_out=None,
                lanes=[
                    LaneInfo(target=LaneTargetInfoParam(param="jaw"), keyframes=2),
                    LaneInfo(target=LaneTargetInfoControl(name="mouth_open"), keyframes=1),
                ],
            )
        ]
    )
