//! Why a `.clm` file was refused.

use crate::formats::clm::ClmError;
use crate::id::NodeId;
use crate::load_budget::LoadLimitError;
use crate::model::ResponseFault;

/// Why reading a model, a fragment or a structure push failed.
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error(".clm codec: {0}")]
    Codec(#[source] ClmError),
    #[error("invalid .clm: {0}")]
    Invalid(#[source] ClmLoadError),
    #[error(transparent)]
    Limit(LoadLimitError),
}

impl From<ClmError> for LoadError {
    /// A limit the codec trips is the same limit the reader trips.
    fn from(error: ClmError) -> Self {
        match error {
            ClmError::LoadLimit(limit) => Self::Limit(limit),
            other => Self::Codec(other),
        }
    }
}

impl From<ClmLoadError> for LoadError {
    fn from(error: ClmLoadError) -> Self {
        Self::Invalid(error)
    }
}

impl From<LoadLimitError> for LoadError {
    fn from(error: LoadLimitError) -> Self {
        Self::Limit(error)
    }
}

impl From<ResponseFault> for LoadError {
    fn from(fault: ResponseFault) -> Self {
        Self::Invalid(match fault {
            ResponseFault::Response { node, reason } => {
                ClmLoadError::InvalidResponse { node, reason }
            }
            ResponseFault::Binding {
                node,
                target,
                reason,
            } => ClmLoadError::InvalidResponseBinding {
                node,
                target,
                reason,
            },
        })
    }
}

/// Every variant names the Id that broke and the field that named it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ClmLoadError {
    #[error("two {kind}s share the id {id:?}")]
    DuplicateId { kind: &'static str, id: String },
    #[error("the file has no root node")]
    NoRoot,
    #[error("node {node:?} has no parent, but {root:?} is already the root")]
    MultipleRoots { root: String, node: String },
    #[error("node {node:?} names parent {parent:?}, which no node has")]
    DanglingParent { node: String, parent: String },
    #[error("node {node:?} names parent {parent:?}, which the file writes after it")]
    NotTopological { node: String, parent: String },
    #[error(
        "node {node:?} has no parent, so this file is a complete model; a fragment's roots name \
         the base node they hang from"
    )]
    FragmentHasNoParent { node: String },
    #[error("node {node:?} names {field} {id:?}, which no node has")]
    DanglingNode {
        node: String,
        field: &'static str,
        id: String,
    },
    #[error("node {node:?} names texture {id:?}, which the file does not carry")]
    DanglingTexture { node: String, id: String },
    #[error(
        "the file carries texture {id:?}, which no part's albedo names; every texture a model \
         holds is drawn by one of its parts, and an addon brings its own"
    )]
    UnusedTexture { id: String },
    #[error("a binding names node {id:?}, which no node has")]
    DanglingBindingNode { id: String },
    #[error("{owner} names param {id:?}, which the file does not carry")]
    DanglingParam { owner: String, id: String },
    #[error(
        "the binding on node {node:?} names {got} params; a binding names one or two distinct ones"
    )]
    BindingParamCount { node: String, got: usize },
    #[error(
        "binding on node {node:?} must carry one nonempty, finite, strictly increasing axis in 0..=1 per param, within the binding-grid budget"
    )]
    BindingPositions { node: String },
    #[error("binding on node {node:?} has invalid authored cell {cell:?}: {reason}")]
    BindingCell {
        node: String,
        cell: [u32; 2],
        reason: &'static str,
    },
    #[error(
        "node {node:?} carries two {target} bindings over the same params; a binding is \
         addressed by its params, node and target, so a model holds one of each"
    )]
    DuplicateBinding { node: String, target: &'static str },
    #[error("part {node:?} carries two vertex slots with the Id {slot:?}")]
    DuplicateSlot { node: String, slot: String },
    #[error(
        "part {node:?} fills vertex slot {slot:?} with vertex {vertex}, past the mesh's {vertices}"
    )]
    SlotOutOfRange {
        node: String,
        slot: String,
        vertex: u32,
        vertices: usize,
    },
    #[error(
        "the {target} binding on node {node:?} targets a mesh group, which is never drawn and \
         has no colour to fold it into"
    )]
    ColorOnMeshGroup { node: String, target: &'static str },
    #[error(
        "the deform binding on node {node:?} targets a {kind}, which carries no mesh to deform"
    )]
    DeformOnUnmeshed { node: String, kind: &'static str },
    #[error(
        "the deform binding on node {node:?} holds {got} offsets at cell {cell:?}, but the node's \
         mesh takes {expected}"
    )]
    DeformCellShape {
        node: String,
        cell: [u32; 2],
        got: usize,
        expected: usize,
    },
    #[error("a weld pairs vertex slot {slot:?} on node {node:?}, which carries no such slot")]
    WeldUnknownSlot { node: String, slot: String },
    #[error("a weld names node {node:?}, which the file does not carry")]
    DanglingWeldEnd { node: String },
    #[error("a weld names node {node:?} on both ends; a weld joins two different parts")]
    WeldSelfPaired { node: String },
    #[error("a weld names node {node:?}, which is a {kind}; a weld joins two parts")]
    WeldEndNotAPart { node: String, kind: &'static str },
    #[error("nodes {a:?} and {b:?} are welded twice; one weld records a pair of parts")]
    DuplicateWeld { a: String, b: String },
    #[error("param {param:?} has a range that is not finite and increasing")]
    ParamRange { param: String },
    #[error("param {param:?} has a default that is not finite")]
    ParamDefault { param: String },
    #[error("node {node:?} has a {field} that is not finite")]
    NodeNotFinite { node: String, field: &'static str },
    #[error("spine {node:?} carries no joints; a spine is at least one link")]
    SpineNoJoints { node: String },
    #[error("spine {node:?} joint {joint}: a coordinate is not finite")]
    SpineJointField { node: String, joint: usize },
    #[error(
        "spine {node:?} joint {joint} repeats the point above it; a link needs a length to turn \
         about"
    )]
    SpineJointCoincident { node: String, joint: usize },
    #[error(
        "spine {node:?} names {targets} targets for {joints} joints; a spine reads one param per \
         link"
    )]
    SpineTargetCount {
        node: String,
        targets: usize,
        joints: usize,
    },
    // The field is `mask_source` rather than `source` because thiserror reads
    // a field of that name as the error's cause.
    #[error(
        "node {node:?} names mask source {mask_source:?}, which is a {kind}; only a part or a \
         composite is drawn, and only what is drawn can be a mask"
    )]
    MaskSourceKind {
        node: String,
        mask_source: String,
        kind: &'static str,
    },
    #[error("node {node:?} names itself as a mask source; a node cannot mask itself")]
    SelfMask { node: String },
    #[error("the mesh on node {node:?} is malformed: {reason}")]
    MalformedMesh { node: String, reason: &'static str },
    #[error("the weld between {a:?} and {b:?} pairs vertex slot {slot:?} twice")]
    WeldSlotPairedTwice { a: String, b: String, slot: String },
    #[error(
        "the weld between {a:?} and {b:?} weights slot {slot:?} outside 0..=1; a weight is a \
         share of a meeting point"
    )]
    WeldWeightOutOfRange { a: String, b: String, slot: String },
    #[error(
        "the structure names texture {id:?}, which the caller could not supply; fetch it before \
         applying the structure"
    )]
    MissingTexture { id: String },
    #[error("invalid motion response {node}: {reason}")]
    InvalidResponse { node: NodeId, reason: String },
    #[error("{target} binding on {node}: {reason}")]
    InvalidResponseBinding {
        node: NodeId,
        target: &'static str,
        reason: String,
    },
}
