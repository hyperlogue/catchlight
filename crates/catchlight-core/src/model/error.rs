//! The one error every model edit returns.

use super::*;

/// Every mutating method leaves the model untouched when it returns one of these.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ModelError {
    #[error("invalid motion response {node}: {reason}")]
    InvalidResponse { node: NodeId, reason: String },
    #[error("{target} binding on {node}: {reason}")]
    InvalidResponseBinding {
        node: NodeId,
        target: &'static str,
        reason: String,
    },
    #[error("unknown node id")]
    UnknownNode,
    #[error("unknown param id")]
    UnknownParam,
    #[error("unknown texture id")]
    UnknownTexture,
    #[error("id {0:?} is already used in this model")]
    DuplicateId(String),
    #[error("could not draw a free id in {ID_MINT_ATTEMPTS} attempts")]
    IdExhausted,
    #[error(transparent)]
    Id(#[from] IdError),
    #[error("cannot reparent a node under itself or a descendant")]
    Cycle,
    #[error("this needs a complete model; an addon fragment has no single root")]
    Fragment,
    #[error("the root node cannot be {0}")]
    Root(&'static str),
    #[error("an in-place kind edit cannot change {0}")]
    KindEdit(&'static str),
    #[error("invalid binding key positions or cell coordinates")]
    CellOutOfRange,
    #[error("a param's range must be finite and increasing")]
    InvalidParamRange,
    #[error("{0} must be finite")]
    NonFinite(&'static str),
    #[error("a deform holds {got} offsets, but the node's mesh takes {expected}")]
    DeformLength { expected: usize, got: usize },
    #[error("no such binding")]
    UnknownBinding,
    #[error("node is not a part")]
    NotAPart,
    #[error("a mask source must be a part or a composite")]
    NotAMaskSource,
    #[error("part carries no such vertex slot")]
    UnknownSlot,
    #[error("vertex slot {0:?} is already on this part")]
    DuplicateSlot(String),
    #[error("a weld pairs two different parts, never a part with itself")]
    WeldSelfPaired,
    #[error("a weld's end is not a part")]
    WeldEndNotAPart,
    #[error("those two parts are already welded; one weld records a part pair")]
    DuplicateWeld,
    #[error("a weld pairs vertex slot {0:?}, which the part on that side does not carry")]
    WeldUnknownSlot(String),
    #[error("a weld pairs vertex slot {0:?} twice")]
    WeldSlotPairedTwice(String),
    #[error("no weld pairs those two parts")]
    UnknownWeld,
    #[error("a vertex slot's weight is a share of a meeting point, so it must be within 0..=1")]
    WeldWeightOutOfRange,
    #[error("node carries no mesh")]
    NotMeshed,
    #[error("node cannot have masks")]
    NotMaskable,
    #[error("a node cannot mask itself")]
    SelfMask,
    #[error("node is not a spine node")]
    NotSpine,
    #[error("node is not a motion response")]
    NotMotionResponse,
    #[error("a spine reads one param per link: {targets} targets for {joints} joints")]
    SpineTargetArity { targets: usize, joints: usize },
    #[error(transparent)]
    SpineJoints(#[from] SpineJointError),
    #[error("a colour binding cannot target a mesh group, which is never drawn")]
    ColorOnMeshGroup,
    #[error("a two-param binding needs two different params")]
    SelfPairedBinding,
    #[error("binding target does not match the operation")]
    WrongTarget,
    #[error("index out of range")]
    IndexOutOfRange,
    #[error("this model carries no extension {0:?}")]
    UnknownExtension(String),
    #[error(
        "extension key {0:?} is catchlight's own to define; a vendor key names its vendor first"
    )]
    ReservedExtension(String),
    #[error("mesh is malformed: {0}")]
    MalformedMesh(&'static str),
    #[error(".clm codec: {0}")]
    Clm(#[from] clm::ClmError),
    #[error("invalid .clm: {0}")]
    InvalidClm(#[from] ClmLoadError),
    #[error(transparent)]
    LoadLimit(#[from] crate::LoadLimitError),
}
