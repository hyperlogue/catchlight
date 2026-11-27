//! Wire types for the catchlight editor.
//!
//! Ids travel as the strings the model file stores, never per-session
//! handles, so [`Command::RenameId`] and texture drops break addons that
//! named the old Id. An add names its new Id under the reply's word (`node`,
//! `param`, …), never `id`: [`Request`] flattens its command beside its own
//! correlation `id`.

pub use catchlight_core::id::{ExtensionKey, NodeId, ParamId, SlotId, TexId};
pub use catchlight_core::{MotionResponse, ResponseInput, ResponseSource};
use serde::{Deserialize, Serialize};

mod command;
mod connection;
mod edit;
mod extension;
mod reply;
mod review;
mod vocab;

pub use command::*;
pub use connection::*;
pub use edit::*;
pub use extension::*;
pub use reply::*;
pub use review::*;
pub use vocab::*;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[serde(transparent)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct SessionId(pub u64);

/// A review thread's identity within its session; never reused.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[serde(transparent)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ThreadId(pub u64);

#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Request {
    pub id: u64,
    #[serde(flatten)]
    pub command: Command,
}

/// The correlation id alone, so a request whose body did not parse is still
/// answered against it.
#[derive(Deserialize, Debug, Clone, Copy)]
pub struct RequestId {
    pub id: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str) -> NodeId {
        NodeId::new(id).expect("valid id")
    }

    fn param(id: &str) -> ParamId {
        ParamId::new(id).expect("valid id")
    }

    #[test]
    fn request_tagging_roundtrips() {
        let req = Request {
            id: 7,
            command: Command::NodeAdd {
                session: SessionId(1),
                parent: node("root"),
                kind: NodeKindArg::Part,
                name: Some("Body".into()),
                node: None,
            },
        };
        let line = serde_json::to_string(&req).unwrap();
        assert!(line.contains("\"cmd\":\"node_add\""));
        assert!(line.contains("\"id\":7"));
        assert!(line.contains("\"parent\":\"root\""), "{line}");
        let back: Request = serde_json::from_str(&line).unwrap();
        assert!(matches!(
            back.command,
            Command::NodeAdd {
                kind: NodeKindArg::Part,
                ..
            }
        ));
    }

    #[test]
    fn ids_travel_as_the_plain_strings_a_model_file_stores() {
        let req = Request {
            id: 1,
            command: Command::BindingCellsSet {
                session: SessionId(2),
                if_rev: 0,
                params: BindingParams::two(param("head.x"), param("head.y")),
                edit: BindingCellsSetArgs {
                    node: node("root/part-3f9a2c1e"),
                    target: BindingTarget::Tx,
                    cells: vec![BindingCellWrite {
                        cell: [1, 2],
                        value: BindingCellValue::Scalar(0.5),
                    }],
                },
            },
        };
        let line = serde_json::to_string(&req).unwrap();
        assert!(line.contains("\"param\":\"head.x\""), "{line}");
        assert!(line.contains("\"param_y\":\"head.y\""), "{line}");
        assert!(line.contains("\"node\":\"root/part-3f9a2c1e\""), "{line}");
        let back: Request = serde_json::from_str(&line).unwrap();
        match back.command {
            Command::BindingCellsSet {
                params,
                edit: BindingCellsSetArgs { cells, .. },
                ..
            } => {
                assert_eq!(params.param.as_str(), "head.x");
                assert_eq!(params.param_y.unwrap().as_str(), "head.y");
                assert_eq!(cells[0].cell, [1, 2]);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_add_names_what_it_makes_rather_than_shadowing_the_requests_id() {
        let line = serde_json::to_string(&Request {
            id: 7,
            command: Command::NodeAdd {
                session: SessionId(1),
                parent: node("root"),
                kind: NodeKindArg::Part,
                name: None,
                node: Some(node("hair")),
            },
        })
        .unwrap();
        assert!(line.contains("\"id\":7"), "{line}");
        assert!(line.contains("\"node\":\"hair\""), "{line}");

        let back: Request = serde_json::from_str(&line).unwrap();
        assert_eq!(back.id, 7);
        match back.command {
            Command::NodeAdd { node, .. } => assert_eq!(node.unwrap().as_str(), "hair"),
            other => panic!("{other:?}"),
        }

        let line = serde_json::to_string(&Request {
            id: 8,
            command: Command::ParamAdd {
                session: SessionId(1),
                name: "Pull".into(),
                min: 0.0,
                max: 1.0,
                default: 0.0,
                param: None,
            },
        })
        .unwrap();
        assert!(!line.contains("\"param\""), "{line}");
    }

    #[test]
    fn an_id_outside_the_charset_does_not_parse() {
        let line = r#"{"id":4,"cmd":"node_delete","session":1,"node":"a b"}"#;
        let err = serde_json::from_str::<Request>(line).unwrap_err();
        assert!(err.to_string().contains("invalid byte"), "{err}");
        let RequestId { id } = serde_json::from_str(line).unwrap();
        assert_eq!(id, 4);
    }

    #[test]
    fn a_misspelt_request_field_is_refused() {
        let line = r#"{"id":4,"cmd":"mesh_set","session":1,"node":"a","verts":[],"uvs":[],"indices":[],"origin":[0,0],"ifrev":5}"#;
        let err = serde_json::from_str::<Request>(line).unwrap_err();
        assert!(err.to_string().contains("unknown field `ifrev`"), "{err}");
        let RequestId { id } = serde_json::from_str(line).unwrap();
        assert_eq!(id, 4);
    }
}
