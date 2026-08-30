//! A puppet: a model being animated.
//!
//! A [`Puppet`] holds everything animating a [`Model`] produces and the model
//! itself never does — the pose, the drivers' state, and the evaluated frame —
//! so one model can back many puppets and posing one never touches the model.

mod arena;

pub use arena::GlobalTransforms;

pub(crate) use arena::Arena;
