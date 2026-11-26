pub mod components;
pub mod deform;
pub mod extension;
pub mod fill;
pub mod formats;
pub mod geometry;
pub mod id;
pub mod interpolate;
pub mod load;
pub mod load_budget;
pub(crate) mod meshgroup;
pub mod model;
pub mod motion_response;
pub mod node;
pub mod puppet;
pub mod spine;
pub mod texture;
pub(crate) mod weld;

pub use components::{
    BlendMode, CompositeData, DecodedTexture, Mask, MaskMode, Mesh, MeshGroupData,
    MeshIndexOutOfRange, MeshIndices, MeshIndicesIter, Node, NodeIdx, NodeKind, PartData,
    TextureIdx, Transform,
};
pub use deform::{DeformShapeError, DeformSource, DeformStack};
pub use id::{
    ExtensionKey, HexSource, IdError, Name, NodeId, NodeIdKind, ParamId, SeededHex, SlotId, TexId,
    EXTENSION_RESERVED_PREFIX, MAX_NAME_BYTES,
};
pub use interpolate::InterpolateMode;
pub use load::{load_model, ModelFormat};
pub use load_budget::{
    LoadBudget, LoadLimitError, LoadLimits, LoadResource, MAX_BINDING_GRID_CELLS,
    MAX_TEXTURE_DIMENSION,
};
pub use model::{
    param_range_is_valid, BindingKey, BindingParams, BindingTarget, CheckWarning, ExtensionValue,
    ExtensionWarning, InstallError, Installed, Model, ModelBinding, ModelBindingValues,
    ModelComposite, ModelError, ModelMask, ModelMesh, ModelMeshGroup, ModelNode, ModelNodeKind,
    ModelParam, ModelPart, ModelSpine, ModelTexture, ModelWeld, ModelWeldPair, NodeProperties,
    Pose, Required, Requirement, Requirements, ScalarTarget, Slot, SlotPair, SpineJointError,
    DEFAULT_SLOT_WEIGHT,
};
pub use motion_response::{MotionResponse, ResponseInput, ResponseSource};
pub use node::{NodeTree, NodeTreeError};
pub use puppet::{
    GlobalTransforms, Motion, ParamLimits, Puppet, ResponseClock, ResponseInputSample,
    ResponseTrace, ScratchTransform,
};
pub use spine::SpineData;
pub use texture::{
    prepare_textures, EncodedTexture, PreppedTexture, TextureError, TextureFormat, UvCrop,
};

pub type Vec2 = glam::Vec2;
pub type Vec3 = glam::Vec3;
pub type Mat2 = glam::Mat2;
pub type Mat4 = glam::Mat4;
