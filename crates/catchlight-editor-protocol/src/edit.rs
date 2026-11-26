//! The edits a command or an atomic batch applies, and the arguments they
//! share.

use serde::{Deserialize, Serialize};

use crate::*;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum Rename {
    Node {
        from: NodeId,
        to: NodeId,
    },
    Param {
        from: ParamId,
        to: ParamId,
    },
    Texture {
        from: TexId,
        to: TexId,
    },
    /// A slot is scoped to its part, so this one names the part too. Every
    /// weld pair that named the old Id follows it.
    Slot {
        node: NodeId,
        from: SlotId,
        to: SlotId,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum BindingCellValue {
    Scalar(f32),
    Offsets(Vec<[f32; 2]>),
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct BindingCellWrite {
    pub cell: [u32; 2],
    pub value: BindingCellValue,
}

/// One convex source contribution to a new mesh vertex's authored deformation.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct VertexWeight {
    pub vertex: u32,
    pub weight: f32,
}

/// Model edits accepted by an atomic batch. Created IDs are always explicit.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum EditOp {
    /// Unlike [`Command::NodeAdd`], names the node it adds.
    NodeAdd {
        parent: NodeId,
        kind: NodeKindArg,
        #[serde(default)]
        name: Option<String>,
        node: NodeId,
    },
    NodeSet {
        node: NodeId,
        #[serde(flatten)]
        patch: NodePatch,
    },
    NodeReparent {
        #[serde(flatten)]
        edit: NodeReparentArgs,
    },
    NodeReorder {
        #[serde(flatten)]
        edit: NodeReorderArgs,
    },
    MeshSet {
        #[serde(flatten)]
        edit: MeshSetArgs,
    },
    BindingKeyInsert {
        #[serde(flatten)]
        params: BindingParams,
        #[serde(flatten)]
        edit: BindingKeyInsertArgs,
    },
    BindingKeyDelete {
        #[serde(flatten)]
        params: BindingParams,
        #[serde(flatten)]
        edit: BindingKeyDeleteArgs,
    },
    BindingKeyMove {
        #[serde(flatten)]
        params: BindingParams,
        #[serde(flatten)]
        edit: BindingKeyMoveArgs,
    },
    BindingAdd {
        #[serde(flatten)]
        params: BindingParams,
        #[serde(flatten)]
        edit: BindingAddArgs,
    },
    BindingCellsSet {
        #[serde(flatten)]
        params: BindingParams,
        #[serde(flatten)]
        edit: BindingCellsSetArgs,
    },
    BindingCellsUnset {
        #[serde(flatten)]
        params: BindingParams,
        #[serde(flatten)]
        edit: BindingCellsUnsetArgs,
    },
    BindingDelete {
        #[serde(flatten)]
        params: BindingParams,
        #[serde(flatten)]
        edit: BindingDeleteArgs,
    },
    BindingInterpolationSet {
        #[serde(flatten)]
        params: BindingParams,
        #[serde(flatten)]
        edit: BindingInterpolationSetArgs,
    },
    MaskAdd {
        #[serde(flatten)]
        edit: MaskAddArgs,
    },
    MaskDelete {
        #[serde(flatten)]
        edit: MaskDeleteArgs,
    },
    /// Unlike [`Command::SlotAdd`], names the slot it adds.
    SlotAdd { node: NodeId, slot: SlotId },
    SlotFill {
        #[serde(flatten)]
        edit: SlotFillArgs,
    },
    SlotClear {
        #[serde(flatten)]
        edit: SlotClearArgs,
    },
    SlotDelete {
        #[serde(flatten)]
        edit: SlotDeleteArgs,
    },
    WeldSet {
        #[serde(flatten)]
        edit: WeldSetArgs,
    },
    WeldDelete {
        #[serde(flatten)]
        edit: WeldDeleteArgs,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct NodeReparentArgs {
    pub node: NodeId,
    pub to: NodeId,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct NodeReorderArgs {
    pub node: NodeId,
    pub index: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct MeshSetArgs {
    pub node: NodeId,
    pub verts: Vec<[f32; 2]>,
    /// One `[u, v]` per vertex, as many as `verts`.
    pub uvs: Vec<[f32; 2]>,
    /// One `[a, b, c]` per triangle, each a `verts` index.
    pub indices: Vec<[u32; 3]>,
    pub origin: [f32; 2],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deform_mapping: Option<Vec<Vec<VertexWeight>>>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct BindingKeyInsertArgs {
    pub node: NodeId,
    pub target: BindingTarget,
    pub axis: ParamId,
    pub value: f32,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct BindingKeyDeleteArgs {
    pub node: NodeId,
    pub target: BindingTarget,
    pub axis: ParamId,
    pub index: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct BindingKeyMoveArgs {
    pub node: NodeId,
    pub target: BindingTarget,
    pub axis: ParamId,
    pub index: u32,
    pub value: f32,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct BindingAddArgs {
    pub node: NodeId,
    pub target: BindingTarget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_positions: Option<Vec<Vec<f32>>>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct BindingCellsSetArgs {
    pub node: NodeId,
    pub target: BindingTarget,
    pub cells: Vec<BindingCellWrite>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct BindingCellsUnsetArgs {
    pub node: NodeId,
    pub target: BindingTarget,
    pub cells: Vec<[u32; 2]>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct BindingDeleteArgs {
    pub node: NodeId,
    pub target: BindingTarget,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct BindingInterpolationSetArgs {
    pub node: NodeId,
    pub target: BindingTarget,
    pub mode: Interpolate,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct MaskAddArgs {
    pub node: NodeId,
    pub source: NodeId,
    pub mode: MaskMode,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct MaskDeleteArgs {
    pub node: NodeId,
    pub index: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct SlotFillArgs {
    pub node: NodeId,
    pub slot: SlotId,
    pub vertex: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct SlotClearArgs {
    pub node: NodeId,
    pub slot: SlotId,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct SlotDeleteArgs {
    pub node: NodeId,
    pub slot: SlotId,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct WeldSetArgs {
    pub a: NodeId,
    pub b: NodeId,
    #[serde(default)]
    pub pairs: Vec<SlotPair>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct WeldDeleteArgs {
    pub a: NodeId,
    pub b: NodeId,
}

/// A command that performs exactly one [`EditOp`]: the op, the session it
/// edits and the revision it expects there.
#[derive(Debug, Clone)]
pub struct SessionEdit {
    pub session: SessionId,
    pub if_rev: Option<u64>,
    pub op: EditOp,
}

impl TryFrom<Command> for SessionEdit {
    /// The command back when it is not one batch op, such as an add that
    /// leaves its new Id to the editor.
    type Error = Command;

    fn try_from(command: Command) -> Result<Self, Command> {
        let (session, if_rev, op) = match command {
            Command::NodeAdd {
                session,
                parent,
                kind,
                name,
                node: Some(node),
            } => (
                session,
                None,
                EditOp::NodeAdd {
                    parent,
                    kind,
                    name,
                    node,
                },
            ),
            Command::NodeSet {
                session,
                node,
                patch,
            } => (session, None, EditOp::NodeSet { node, patch }),
            Command::NodeReparent { session, edit } => {
                (session, None, EditOp::NodeReparent { edit })
            }
            Command::NodeReorder { session, edit } => (session, None, EditOp::NodeReorder { edit }),
            Command::MaskAdd { session, edit } => (session, None, EditOp::MaskAdd { edit }),
            Command::MaskDelete { session, edit } => (session, None, EditOp::MaskDelete { edit }),
            Command::MeshSet {
                session,
                if_rev,
                edit,
            } => (session, if_rev, EditOp::MeshSet { edit }),
            Command::BindingKeyInsert {
                session,
                if_rev,
                params,
                edit,
            } => (session, if_rev, EditOp::BindingKeyInsert { params, edit }),
            Command::BindingKeyDelete {
                session,
                if_rev,
                params,
                edit,
            } => (session, if_rev, EditOp::BindingKeyDelete { params, edit }),
            Command::BindingKeyMove {
                session,
                if_rev,
                params,
                edit,
            } => (session, if_rev, EditOp::BindingKeyMove { params, edit }),
            Command::BindingCellsSet {
                session,
                if_rev,
                params,
                edit,
            } => (
                session,
                Some(if_rev),
                EditOp::BindingCellsSet { params, edit },
            ),
            Command::BindingCellsUnset {
                session,
                if_rev,
                params,
                edit,
            } => (
                session,
                Some(if_rev),
                EditOp::BindingCellsUnset { params, edit },
            ),
            Command::BindingAdd {
                session,
                params,
                edit,
            } => (session, None, EditOp::BindingAdd { params, edit }),
            Command::BindingDelete {
                session,
                params,
                edit,
            } => (session, None, EditOp::BindingDelete { params, edit }),
            Command::BindingInterpolationSet {
                session,
                params,
                edit,
            } => (
                session,
                None,
                EditOp::BindingInterpolationSet { params, edit },
            ),
            Command::SlotAdd {
                session,
                node,
                slot: Some(slot),
            } => (session, None, EditOp::SlotAdd { node, slot }),
            Command::SlotFill { session, edit } => (session, None, EditOp::SlotFill { edit }),
            Command::SlotClear { session, edit } => (session, None, EditOp::SlotClear { edit }),
            Command::SlotDelete { session, edit } => (session, None, EditOp::SlotDelete { edit }),
            Command::WeldSet { session, edit } => (session, None, EditOp::WeldSet { edit }),
            Command::WeldDelete { session, edit } => (session, None, EditOp::WeldDelete { edit }),
            other => return Err(other),
        };
        Ok(Self {
            session,
            if_rev,
            op,
        })
    }
}

/// Exactly one source and its declared attachments initialize a new session.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "format", rename_all = "snake_case", deny_unknown_fields)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum SessionSource {
    Clm {},
    Json {
        #[serde(default)]
        textures: Vec<ImportTexture>,
    },
    Manifest {},
}

/// The param, or the pair of params, a binding is keyed by. With `param_y`
/// the binding owns two position axes and `cell` indexes both; without it
/// the grid is one row and `cell[1]` is 0.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct BindingParams {
    pub param: ParamId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub param_y: Option<ParamId>,
}

impl BindingParams {
    #[must_use]
    pub const fn one(param: ParamId) -> Self {
        Self {
            param,
            param_y: None,
        }
    }

    #[must_use]
    pub const fn two(x: ParamId, y: ParamId) -> Self {
        Self {
            param: x,
            param_y: Some(y),
        }
    }
}

/// How [`Command::MeshAuto`] derives a mesh from a part's alpha. Absent knobs
/// take `catchlight-editor-core`'s defaults.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum AutoMesh {
    Contour {
        /// Alpha strictly above this counts as solid.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        threshold: Option<u8>,
        /// Douglas-Peucker tolerance, in texels: higher is a coarser outline.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        simplify: Option<f32>,
        /// Texels of outward dilation before tracing, so the outline clears
        /// the art's own edge.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        margin: Option<u32>,
        /// Interior fill-point spacing in texels; 0 is boundary only.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        spacing: Option<u32>,
        /// Ring scale factors about the outline's centroid: 0 the centroid, 1 the
        /// outline; above 1 clamps to 1.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rings: Option<Vec<f32>>,
        /// Texels: a free vertex closer than this to a placed one is dropped.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min_distance: Option<f32>,
        /// Texel x of a vertical mirror line for free vertices; the outline is not
        /// mirrored.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mirror_x: Option<f32>,
    },
    /// A grid over the bounding box of the solid texels.
    Grid {
        /// Alpha strictly above this counts as solid.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        threshold: Option<u8>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cols: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rows: Option<u32>,
        /// Grid lines as fractions of the solid bounding box (0 left, 1 right);
        /// present, replaces `cols` and `margin` on this axis.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        axes_x: Option<Vec<f32>>,
        /// The same, down, replacing `rows`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        axes_y: Option<Vec<f32>>,
        /// Fraction of the bounding box added outside it on both sides, when
        /// the lines come from `cols`/`rows`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        margin: Option<f32>,
    },
}

impl Default for AutoMesh {
    fn default() -> Self {
        Self::Contour {
            threshold: None,
            simplify: None,
            margin: None,
            spacing: None,
            rings: None,
            min_distance: None,
            mirror_x: None,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct SlotAddr {
    pub node: NodeId,
    pub slot: SlotId,
}

/// `weight` is `a`'s share (`0..=1`) of the point both vertices are pulled to.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct SlotPair {
    pub a: SlotId,
    pub b: SlotId,
    pub weight: f32,
}

/// A present JSON value — `null` included — as `Some`, for a field whose
/// absence and whose `null` mean different things (the merge-patch idiom).
fn present_as_some<'de, D, T>(de: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(de).map(Some)
}

/// Fields to change on a node; absent = unchanged. Colour fields reach parts
/// and composites only.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct NodePatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub translate: Option<[f32; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotate: Option<[f32; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<[f32; 2]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub z_order: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Merge patch: absent leaves it, `null` draws none, an Id draws that one.
    #[serde(
        default,
        deserialize_with = "present_as_some",
        skip_serializing_if = "Option::is_none"
    )]
    // ts-rs would otherwise spell the double option `TexId | null | null`.
    #[cfg_attr(feature = "ts", ts(as = "Option<TexId>"))]
    pub texture: Option<Option<TexId>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock_to_root: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blend_mode: Option<BlendMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tint: Option<[f32; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screen_tint: Option<[f32; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask_threshold: Option<f32>,
    /// Composite: forward mesh-group deformation to children.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub propagate_meshgroup: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mg_translate_children: Option<bool>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ParamPose {
    pub param: ParamId,
    pub value: f32,
}

/// One texture an [`Command::ImportJson`] structure names, arriving as
/// attachment `texture:<texture>`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ImportTexture {
    pub texture: TexId,
    #[serde(default)]
    pub encoding: TextureEncoding,
    #[serde(default)]
    pub alpha: TextureAlpha,
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
    fn a_one_param_binding_leaves_the_second_param_off_the_wire() {
        let line = serde_json::to_string(&Request {
            id: 1,
            command: Command::BindingAdd {
                session: SessionId(1),
                params: BindingParams::one(param("pull")),
                edit: BindingAddArgs {
                    key_positions: None,
                    node: node("body"),
                    target: BindingTarget::Tx,
                },
            },
        })
        .unwrap();
        assert!(!line.contains("param_y"), "{line}");
        let back: Request = serde_json::from_str(&line).unwrap();
        match back.command {
            Command::BindingAdd { params, .. } => assert!(params.param_y.is_none()),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_misspelt_field_is_refused_inside_a_flattened_or_nested_payload() {
        for line in [
            r#"{"id":1,"cmd":"node_set","session":1,"node":"a","nmae":"b"}"#,
            r#"{"id":1,"cmd":"binding_add","session":1,"node":"a","target":"opacity","param":"p","paramy":"q"}"#,
            r#"{"id":1,"cmd":"edit_apply","session":1,"if_rev":1,"edits":[{"op":"node_set","node":"a","nmae":"b"}]}"#,
            r#"{"id":1,"cmd":"mesh_set","session":1,"node":"a","verts":[],"uvs":[],"indices":[],"origin":[0,0],"orgin":[0,0]}"#,
            r#"{"id":1,"cmd":"binding_key_insert","session":1,"node":"a","target":"opacity","param":"p","axis":"p","value":0,"vlaue":0}"#,
            r#"{"id":1,"cmd":"edit_apply","session":1,"if_rev":1,"edits":[{"op":"weld_set","a":"a","b":"b","pars":[]}]}"#,
            r#"{"id":1,"cmd":"mesh_generate","session":1,"node":"a","mode":{"mode":"grid","colls":4}}"#,
        ] {
            let err = serde_json::from_str::<Request>(line).unwrap_err();
            assert!(err.to_string().contains("unknown field"), "{line}: {err}");
        }
    }

    #[test]
    fn a_batch_op_omits_and_defaults_what_its_command_does() {
        let op = EditOp::MeshSet {
            edit: MeshSetArgs {
                node: node("a"),
                verts: Vec::new(),
                uvs: Vec::new(),
                indices: Vec::new(),
                origin: [0.0, 0.0],
                deform_mapping: None,
            },
        };
        let line = serde_json::to_string(&op).unwrap();
        assert!(!line.contains("deform_mapping"), "{line}");
        let weld: EditOp = serde_json::from_str(r#"{"op":"weld_set","a":"a","b":"b"}"#).unwrap();
        match weld {
            EditOp::WeldSet { edit } => assert!(edit.pairs.is_empty()),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_add_is_one_batch_op_only_when_it_names_its_id() {
        let add = |slot| Command::SlotAdd {
            session: SessionId(1),
            node: node("a"),
            slot,
        };
        match SessionEdit::try_from(add(Some(SlotId::new("s").unwrap()))) {
            Ok(SessionEdit {
                op: EditOp::SlotAdd { slot, .. },
                if_rev: None,
                ..
            }) => assert_eq!(slot.as_str(), "s"),
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            SessionEdit::try_from(add(None)),
            Err(Command::SlotAdd { slot: None, .. })
        ));
    }

    #[test]
    fn a_rename_names_the_kind_it_renames() {
        let s = serde_json::to_string(&Command::RenameId {
            session: SessionId(1),
            rename: Rename::Param {
                from: param("param-0001"),
                to: param("pull"),
            },
        })
        .unwrap();
        assert!(s.contains("\"kind\":\"param\""), "{s}");
        let back: Command = serde_json::from_str(&s).unwrap();
        match back {
            Command::RenameId {
                rename: Rename::Param { to, .. },
                ..
            } => assert_eq!(to.as_str(), "pull"),
            other => panic!("{other:?}"),
        }
    }
}
