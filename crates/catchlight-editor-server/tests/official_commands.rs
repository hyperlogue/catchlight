#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a test aborts on a broken fixture"
)]

//! The official extensions' typed commands: each edit is one undo step that
//! refuses what the extension crates' checked functions refuse, and each read
//! is answered from the model alone.

use catchlight_core::extension::Extension;
use catchlight_core::formats::clm::{self, ClmExtension, ClmPayload};
use catchlight_core::{ExtensionKey, InterpolateMode, Model, ModelParam, Name};
use catchlight_editor_protocol::{
    ClipInfo, Command, ControlMappingInfo, ErrorCode, ExpressionGroup, ExpressionInfo,
    ExpressionSetting, ExtensionRef, ExtensionSet, ExtensionWarningInfo, LaneInfo, LaneTargetInfo,
    ParamId, Reply, Request, ResponseBody, ReviewAuthor, SessionId, SessionSource,
};
use catchlight_editor_server::{Attachments, Editor, replica_query};
use catchlight_extension_animations::{Animations, Clip, Keyframe, Lane, LaneTarget};
use catchlight_extension_expressions::{EMOTIONS, Expressions, VISEMES};
use catchlight_extension_humanoid::{CONTROLS, Humanoid};
use serde_json::json;

struct Fixture {
    editor: Editor,
    session: SessionId,
    next: u64,
}

fn pid(s: &str) -> ParamId {
    ParamId::new(s).unwrap()
}

/// `jaw` in 0..1 resting at 0.2, `turn` in -2..2 resting at -2.
fn rig() -> Model {
    let mut model = Model::new();
    for (p, min, max, default) in [("jaw", 0.0, 1.0, 0.2), ("turn", -2.0, 2.0, -2.0)] {
        model
            .add_param_with_id(
                pid(p),
                ModelParam::new(Name::truncated(p), min, max, default),
            )
            .unwrap();
    }
    model
}

impl Fixture {
    fn new() -> Self {
        Self::with(&rig())
    }

    fn with(model: &Model) -> Self {
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
            other => panic!("session_create: {other:?}"),
        };
        Self {
            editor,
            session,
            next: 1,
        }
    }

    fn send(&mut self, command: Command) -> Reply {
        self.next += 1;
        self.editor.handle(Request {
            id: self.next,
            command,
        })
    }

    fn ok(&mut self, command: Command) -> ResponseBody {
        match self.send(command) {
            Reply::Ok { body, .. } => body,
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    fn code(&mut self, command: Command) -> ErrorCode {
        match self.send(command) {
            Reply::Err { code, .. } => code,
            other => panic!("expected Err, got {other:?}"),
        }
    }

    fn rev(&mut self) -> u64 {
        match self.ok(Command::Status {
            session: self.session,
        }) {
            ResponseBody::Status { status } => status.rev,
            other => panic!("{other:?}"),
        }
    }

    fn undo(&mut self) {
        let if_rev = self.rev();
        self.ok(Command::Undo {
            session: self.session,
            if_rev,
        });
    }

    fn model(&self) -> Model {
        self.editor.with_model(self.session, Clone::clone).unwrap()
    }

    fn map(&self, control: &str, param: &str) -> Command {
        Command::HumanoidMap {
            session: self.session,
            control: control.into(),
            param: pid(param),
            stops: None,
            nonstandard: false,
        }
    }

    fn set(&self, group: ExpressionGroup, name: &str, settings: Vec<ExpressionSetting>) -> Command {
        Command::ExpressionSet {
            session: self.session,
            group,
            name: name.into(),
            settings,
            nonstandard: false,
        }
    }

    fn humanoid(&mut self) -> (Vec<ControlMappingInfo>, usize) {
        match self.ok(Command::HumanoidGet {
            session: self.session,
        }) {
            ResponseBody::Humanoid { mappings, controls } => (mappings, controls.len()),
            other => panic!("{other:?}"),
        }
    }

    fn expressions(&mut self) -> (Vec<ExpressionInfo>, Vec<ExpressionInfo>) {
        match self.ok(Command::ExpressionList {
            session: self.session,
        }) {
            ResponseBody::Expressions {
                expressions,
                visemes,
                ..
            } => (expressions, visemes),
            other => panic!("{other:?}"),
        }
    }

    fn warnings(&mut self) -> Vec<ExtensionWarningInfo> {
        match self.ok(Command::ExtensionWarnings {
            session: self.session,
        }) {
            ResponseBody::ExtensionWarnings { warnings } => warnings,
            other => panic!("{other:?}"),
        }
    }
}

fn control(name: &str, value: f32) -> ExpressionSetting {
    ExpressionSetting {
        control: Some(name.into()),
        param: None,
        value,
    }
}

fn param(id: &str, value: f32) -> ExpressionSetting {
    ExpressionSetting {
        control: None,
        param: Some(pid(id)),
        value,
    }
}

fn mapping(control: &str, param: &str, stops: [f32; 3]) -> ControlMappingInfo {
    ControlMappingInfo {
        control: control.into(),
        param: pid(param),
        stops,
    }
}

#[test]
fn humanoid_map_is_one_undo_step_and_a_repeat_keeps_the_revision() {
    let mut f = Fixture::new();
    let before = f.rev();
    let body = f.ok(f.map("head_yaw", "turn"));
    let mapped = mapping("head_yaw", "turn", [-2.0, -2.0, 2.0]);
    match body {
        ResponseBody::HumanoidMapped { mapping, replaced } => {
            assert_eq!(mapping, mapped);
            assert_eq!(replaced, None);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(f.rev(), before + 1);
    assert_eq!(f.humanoid().0, std::slice::from_ref(&mapped));

    f.ok(f.map("head_yaw", "turn"));
    assert_eq!(
        f.rev(),
        before + 1,
        "the same mapping again changes nothing"
    );

    let body = f.ok(Command::HumanoidMap {
        session: f.session,
        control: "mouth_open".into(),
        param: pid("jaw"),
        stops: Some([0.0, 0.5, 1.0]),
        nonstandard: false,
    });
    assert!(
        matches!(body, ResponseBody::HumanoidMapped { mapping, .. } if mapping.stops == [0.0, 0.5, 1.0])
    );
    f.undo();
    assert_eq!(f.humanoid().0, [mapped], "undo takes back one map");
}

#[test]
fn humanoid_map_refuses_what_the_extension_refuses() {
    let mut f = Fixture::new();
    let before = f.rev();
    assert_eq!(f.code(f.map("head_yaw", "ghost")), ErrorCode::NoParam);
    assert_eq!(
        f.code(f.map("head_yaww", "turn")),
        ErrorCode::NonstandardControl
    );
    match f.send(f.map("head_yaww", "turn")) {
        Reply::Err { message, .. } => assert!(message.contains("head_yaw\""), "{message}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(f.code(f.map("", "turn")), ErrorCode::BadRequest);
    assert_eq!(f.rev(), before);
    assert!(f.model().extensions().is_empty());

    f.ok(Command::HumanoidMap {
        session: f.session,
        control: "tail_wag".into(),
        param: pid("jaw"),
        stops: None,
        nonstandard: true,
    });
    assert_eq!(f.humanoid().0[0].control, "tail_wag");
}

#[test]
fn humanoid_unmap_removes_the_extension_with_the_last_mapping() {
    let mut f = Fixture::new();
    let unmap = |f: &Fixture, control: &str| Command::HumanoidUnmap {
        session: f.session,
        control: control.into(),
    };
    assert_eq!(f.code(unmap(&f, "head_yaw")), ErrorCode::Edit);
    f.ok(f.map("head_yaw", "turn"));
    f.ok(f.map("mouth_open", "jaw"));
    let before = f.rev();

    match f.ok(unmap(&f, "head_yaw")) {
        ResponseBody::HumanoidUnmapped { mapping: removed } => {
            assert_eq!(removed, mapping("head_yaw", "turn", [-2.0, -2.0, 2.0]));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(f.rev(), before + 1);
    f.ok(unmap(&f, "mouth_open"));
    assert!(!f.model().extensions().contains_key(Humanoid::KEY));
    f.undo();
    assert!(f.model().extensions().contains_key(Humanoid::KEY));
}

#[test]
fn expression_set_and_delete_edit_one_group_each_one_undo_step() {
    let mut f = Fixture::new();
    let sad = vec![param("jaw", 0.5), control("mouth_smile", -0.5)];
    let before = f.rev();
    match f.ok(f.set(ExpressionGroup::Expression, "sad", sad.clone())) {
        ResponseBody::Expression {
            group,
            expression,
            replaced,
        } => {
            assert_eq!(group, ExpressionGroup::Expression);
            assert_eq!(expression.settings, sad, "in the order given");
            assert_eq!(replaced, None);
        }
        other => panic!("{other:?}"),
    }
    f.ok(f.set(ExpressionGroup::Expression, "sad", sad.clone()));
    assert_eq!(
        f.rev(),
        before + 1,
        "the same expression again changes nothing"
    );

    f.ok(f.set(
        ExpressionGroup::Viseme,
        "aa",
        vec![control("mouth_open", 1.0)],
    ));
    let (expressions, visemes) = f.expressions();
    assert_eq!(expressions.len(), 1);
    assert_eq!(visemes[0].name, "aa");

    match f.ok(f.set(
        ExpressionGroup::Expression,
        "sad",
        vec![control("mouth_open", 0.2)],
    )) {
        ResponseBody::Expression { replaced, .. } => assert_eq!(replaced.unwrap().settings, sad),
        other => panic!("{other:?}"),
    }
    f.undo();
    assert_eq!(f.expressions().0[0].settings, sad);

    let delete = |f: &Fixture, group, name: &str| Command::ExpressionDelete {
        session: f.session,
        group,
        name: name.into(),
    };
    assert_eq!(
        f.code(delete(&f, ExpressionGroup::Viseme, "sad")),
        ErrorCode::Edit
    );
    match f.ok(delete(&f, ExpressionGroup::Viseme, "aa")) {
        ResponseBody::ExpressionDeleted { group, expression } => {
            assert_eq!(group, ExpressionGroup::Viseme);
            assert_eq!(expression.name, "aa");
        }
        other => panic!("{other:?}"),
    }
    f.ok(delete(&f, ExpressionGroup::Expression, "sad"));
    assert!(!f.model().extensions().contains_key(Expressions::KEY));
}

#[test]
fn expression_set_refuses_what_the_extension_refuses() {
    let mut f = Fixture::new();
    let before = f.rev();
    let group = ExpressionGroup::Expression;
    for (settings, name, code) in [
        (vec![control("mouth_open", 0.2)], "", ErrorCode::BadRequest),
        (Vec::new(), "x", ErrorCode::BadRequest),
        (
            vec![control("mouth_open", -0.5)],
            "x",
            ErrorCode::BadRequest,
        ),
        (
            vec![control("mouth_opn", 0.5)],
            "x",
            ErrorCode::NonstandardControl,
        ),
        (vec![param("ghost", 0.5)], "x", ErrorCode::NoParam),
        (
            vec![ExpressionSetting {
                control: Some("mouth_open".into()),
                param: Some(pid("jaw")),
                value: 0.5,
            }],
            "x",
            ErrorCode::BadRequest,
        ),
    ] {
        assert_eq!(
            f.code(f.set(group, name, settings.clone())),
            code,
            "{settings:?}"
        );
    }
    assert_eq!(f.rev(), before);
    assert!(f.model().extensions().is_empty());
}

#[test]
fn viseme_add_standard_reports_what_it_added_replaced_and_skipped() {
    let mut f = Fixture::new();
    f.ok(f.set(
        ExpressionGroup::Viseme,
        "aa",
        vec![control("mouth_open", 0.1)],
    ));
    let add = |f: &Fixture, replace| Command::VisemeAddStandard {
        session: f.session,
        replace,
    };
    match f.ok(add(&f, false)) {
        ResponseBody::VisemesAdded {
            added,
            replaced,
            skipped,
        } => {
            assert_eq!(skipped, ["aa"]);
            assert!(replaced.is_empty());
            assert_eq!(added.len(), VISEMES.len() - 1);
        }
        other => panic!("{other:?}"),
    }
    let before = f.rev();
    f.ok(add(&f, false));
    assert_eq!(f.rev(), before, "skipping every viseme changes nothing");
    match f.ok(add(&f, true)) {
        ResponseBody::VisemesAdded { replaced, .. } => assert_eq!(replaced.len(), VISEMES.len()),
        other => panic!("{other:?}"),
    }
    assert_eq!(f.rev(), before + 1);
}

#[test]
fn the_reads_answer_on_a_model_without_the_extensions() {
    let mut f = Fixture::new();
    let (mappings, controls) = f.humanoid();
    assert!(mappings.is_empty());
    assert_eq!(controls, CONTROLS.len());
    match f.ok(Command::ExpressionList { session: f.session }) {
        ResponseBody::Expressions {
            expressions,
            visemes,
            emotion_names,
            viseme_names,
        } => {
            assert!(expressions.is_empty() && visemes.is_empty());
            assert_eq!(emotion_names, EMOTIONS);
            assert_eq!(viseme_names, VISEMES);
        }
        other => panic!("{other:?}"),
    }
    assert!(f.warnings().is_empty());
}

#[test]
fn extension_warnings_are_structured_with_their_subjects() {
    let mut f = Fixture::new();
    f.ok(f.map("head_yaw", "turn"));
    f.ok(f.map("head_pitch", "turn"));
    f.ok(Command::ExtensionSet {
        session: f.session,
        key: "acme.framing".parse().unwrap(),
        value: ExtensionSet::Maintained {
            version: 1,
            refs: vec![ExtensionRef::Param(pid("ghost"))],
            value: Some(json!({})),
        },
    });
    let warnings = f.warnings();
    let shared = warnings
        .iter()
        .find(|w| w.code == "catchlight.humanoid/shared-param")
        .unwrap();
    assert_eq!(shared.key.as_str(), Humanoid::KEY);
    assert_eq!(shared.subjects, [ExtensionRef::Param(pid("turn"))]);
    let dangling = warnings
        .iter()
        .find(|w| w.key.as_str() == "acme.framing" && w.code == "catchlight/dangling-reference")
        .unwrap();
    assert_eq!(dangling.subjects, [ExtensionRef::Param(pid("ghost"))]);
    assert!(
        warnings
            .iter()
            .any(|w| w.key.as_str() == "acme.framing" && w.code == "catchlight/not-checked"),
        "{warnings:?}"
    );
    assert!(
        !warnings
            .iter()
            .any(|w| w.key.as_str() == Humanoid::KEY && w.code == "catchlight/not-checked"),
        "a session checks the official extensions"
    );
}

#[test]
fn official_data_this_editor_cannot_read_is_neither_shown_nor_edited() {
    let mut file = clm::decode(&rig().to_clm_bytes().unwrap()).unwrap();
    for key in [Humanoid::KEY, Expressions::KEY, Animations::KEY] {
        file.doc.extensions.insert(
            ExtensionKey::new(key).unwrap(),
            ClmExtension::Maintained {
                version: 99,
                refs: Vec::new(),
                payload: ClmPayload::Json(json!({"future": true})),
            },
        );
    }
    let model = Model::from_clm_bytes(&file.encode().unwrap()).unwrap();
    let mut f = Fixture::with(&model);
    let session = f.session;
    for command in [
        Command::HumanoidGet { session },
        Command::ExpressionList { session },
        Command::ClipList { session },
        f.map("head_yaw", "turn"),
        f.set(
            ExpressionGroup::Viseme,
            "aa",
            vec![control("mouth_open", 1.0)],
        ),
        Command::VisemeAddStandard {
            session,
            replace: false,
        },
    ] {
        let tag = command.tag();
        assert_eq!(f.code(command), ErrorCode::ExtensionUnread, "{tag}");
    }
    assert!(
        f.warnings()
            .iter()
            .any(|w| w.code == "catchlight/newer-version")
    );
}

#[test]
fn a_read_only_session_refuses_every_typed_edit() {
    let mut f = Fixture::new();
    f.ok(Command::VersionSave {
        session: f.session,
        label: None,
        summary: None,
        author: ReviewAuthor::Agent {
            id: "a".into(),
            name: "agent".into(),
        },
        key: None,
    });
    let opened = match f.ok(Command::VersionOpen {
        session: f.session,
        version: 1,
    }) {
        ResponseBody::Session { session } => session,
        other => panic!("{other:?}"),
    };
    f.session = opened;
    for command in [
        f.map("head_yaw", "turn"),
        Command::HumanoidUnmap {
            session: opened,
            control: "head_yaw".into(),
        },
        f.set(
            ExpressionGroup::Expression,
            "sad",
            vec![control("mouth_open", 0.2)],
        ),
        Command::ExpressionDelete {
            session: opened,
            group: ExpressionGroup::Expression,
            name: "sad".into(),
        },
        Command::VisemeAddStandard {
            session: opened,
            replace: false,
        },
    ] {
        let tag = command.tag();
        assert_eq!(f.code(command), ErrorCode::ReadOnly, "{tag}");
    }
    f.humanoid();
}

#[test]
fn a_replica_answers_the_typed_reads_as_the_editor_does() {
    let mut f = Fixture::new();
    f.ok(f.map("head_yaw", "turn"));
    f.ok(f.set(ExpressionGroup::Expression, "sad", vec![param("jaw", 0.5)]));
    let session = f.session;
    for command in [
        Command::HumanoidGet { session },
        Command::ExpressionList { session },
        Command::ExtensionWarnings { session },
        Command::ClipList { session },
    ] {
        let from_editor = f.ok(command.clone());
        let from_replica = replica_query(&f.model(), &command).unwrap();
        assert_eq!(
            serde_json::to_value(&from_editor).unwrap(),
            serde_json::to_value(&from_replica).unwrap(),
            "{}",
            command.tag()
        );
    }
}

fn lane(target: LaneTarget, frames: &[u32]) -> Lane {
    Lane::new(
        target,
        InterpolateMode::Linear,
        frames
            .iter()
            .map(|&frame| Keyframe { frame, value: 0.5 })
            .collect(),
    )
}

#[test]
fn clip_list_reads_each_clip_with_what_its_lanes_drive() {
    let mut f = Fixture::new();
    let session = f.session;
    let clips = |body: ResponseBody| match body {
        ResponseBody::Clips { clips } => clips,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        clips(f.ok(Command::ClipList { session })),
        [],
        "a model without clips"
    );

    let mut model = rig();
    *model.extension_or_default::<Animations>().unwrap() = Animations::from_clips([
        Clip {
            timestep: 0.1,
            length: 12,
            lead_in: Some(2),
            lead_out: Some(10),
            lanes: vec![
                lane(LaneTarget::Param(pid("jaw")), &[0, 11]),
                lane(LaneTarget::Control("mouth_open".into()), &[3]),
                lane(LaneTarget::Expression("happy".into()), &[0, 4, 8]),
                lane(LaneTarget::Viseme("aa".into()), &[]),
            ],
            ..Clip::new("talk")
        },
        Clip::new("idle"),
    ]);
    let mut f = Fixture::with(&model);
    let session = f.session;
    let lanes = |targets: Vec<(LaneTargetInfo, u32)>| {
        targets
            .into_iter()
            .map(|(target, keyframes)| LaneInfo { target, keyframes })
            .collect()
    };
    let name = |s: &str| s.to_string();
    let want = vec![
        ClipInfo {
            name: name("talk"),
            timestep: 0.1,
            length: 12,
            lead_in: Some(2),
            lead_out: Some(10),
            lanes: lanes(vec![
                (LaneTargetInfo::Param { param: pid("jaw") }, 2),
                (
                    LaneTargetInfo::Control {
                        name: name("mouth_open"),
                    },
                    1,
                ),
                (
                    LaneTargetInfo::Expression {
                        name: name("happy"),
                    },
                    3,
                ),
                (LaneTargetInfo::Viseme { name: name("aa") }, 0),
            ]),
        },
        ClipInfo {
            name: name("idle"),
            timestep: 1.0 / 60.0,
            length: 0,
            lead_in: None,
            lead_out: None,
            lanes: Vec::new(),
        },
    ];
    assert_eq!(clips(f.ok(Command::ClipList { session })), want);
    assert_eq!(
        clips(replica_query(&f.model(), &Command::ClipList { session }).unwrap()),
        want
    );
    let wire = serde_json::to_value(&want).unwrap();
    assert_eq!(
        wire[0]["lanes"][0],
        json!({"target": {"kind": "param", "param": "jaw"}, "keyframes": 2})
    );
}
