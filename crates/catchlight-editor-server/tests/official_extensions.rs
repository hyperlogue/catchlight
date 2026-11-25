#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! A session checks the official extensions by their own rules: building one
//! registers them.

use catchlight_core::{InterpolateMode, Model, ParamId};
use catchlight_editor_protocol::{Command, Reply, Request, ResponseBody, SessionSource};
use catchlight_editor_server::{Attachments, Editor};
use catchlight_extension_animations::{Animations, Clip, Keyframe, Lane};

#[test]
fn a_session_checks_its_animations_as_the_extension_does() {
    let mut model = Model::new();
    *model.extension_or_default::<Animations>().unwrap() = Animations::from_clips([Clip {
        length: 2,
        lanes: vec![Lane {
            param: ParamId::new("ghost").unwrap(),
            interpolation: InterpolateMode::Linear,
            keyframes: vec![Keyframe {
                frame: 0,
                value: 1.0,
            }],
        }],
        ..Clip::new("haunt")
    }]);

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
    let warnings = match editor.handle(Request {
        id: 2,
        command: Command::Check { session },
    }) {
        Reply::Ok {
            body: ResponseBody::Warnings { warnings },
            ..
        } => warnings,
        other => panic!("check: {other:?}"),
    };
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
