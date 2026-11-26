#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! A session checks the official extensions by their own rules: building one
//! registers them.

use catchlight_core::{InterpolateMode, Model, ParamId};
use catchlight_editor_protocol::{Command, Reply, Request, ResponseBody, SessionSource};
use catchlight_editor_server::{Attachments, Editor};
use catchlight_extension_animations::{Animations, Clip, Keyframe, Lane, LaneTarget};
use catchlight_extension_expressions::{Expression, Expressions, Setting};
use catchlight_extension_humanoid::{ControlMapping, Humanoid};

#[test]
fn a_session_checks_its_animations_as_the_extension_does() {
    let mut model = Model::new();
    *model.extension_or_default::<Animations>().unwrap() = Animations::from_clips([Clip {
        length: 2,
        lanes: vec![Lane::new(
            LaneTarget::Param(ParamId::new("ghost").unwrap()),
            InterpolateMode::Linear,
            vec![Keyframe {
                frame: 0,
                value: 1.0,
            }],
        )],
        ..Clip::new("haunt")
    }]);

    let warnings = check_in_a_session(&model);
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("catchlight.animations") && w.contains("ghost")),
        "{warnings:?}"
    );
    assert!(
        !warnings.iter().any(|w| w.contains("not registered")),
        "{warnings:?}"
    );
}

fn check_in_a_session(model: &Model) -> Vec<String> {
    let editor = Editor::new();
    let mut attachments = Attachments::none();
    attachments.insert("model", model.to_clm_bytes().unwrap());
    let session = match editor
        .handle_with(
            Request {
                id: 1,
                command: Command::SessionNew {
                    name: None,
                    source: Some(SessionSource::Clm {}),
                },
            },
            attachments,
        )
        .0
    {
        Reply::Ok {
            body: ResponseBody::Session { session },
            ..
        } => session,
        other => panic!("session_new: {other:?}"),
    };
    match editor.handle(Request {
        id: 2,
        command: Command::Check { session },
    }) {
        Reply::Ok {
            body: ResponseBody::Warnings { warnings },
            ..
        } => warnings,
        other => panic!("check: {other:?}"),
    }
}

#[test]
fn a_session_checks_its_humanoid_and_expressions_as_the_extensions_do() {
    let mut model = Model::new();
    model.extension_or_default::<Humanoid>().unwrap().set(
        "tail_wag",
        ControlMapping {
            param: ParamId::new("ghost").unwrap(),
            stops: [-1.0, 0.0, 1.0],
        },
    );
    *model.extension_or_default::<Expressions>().unwrap() =
        Expressions::from_expressions([Expression::new(
            "wag",
            vec![Setting::control("tail_wag", 2.0)],
        )]);
    let warnings = check_in_a_session(&model);
    for key in ["catchlight.humanoid", "catchlight.expressions"] {
        assert!(
            warnings
                .iter()
                .any(|w| w.contains(key) && w.contains("tail_wag") && w.contains("standard")),
            "{key}: {warnings:?}"
        );
    }
    assert!(
        !warnings.iter().any(|w| w.contains("not registered")),
        "{warnings:?}"
    );
}
