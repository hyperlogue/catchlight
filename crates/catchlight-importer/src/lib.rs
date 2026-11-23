//! External format adapters that import into a catchlight [`Model`](catchlight_core::Model).
//!
//! `.inx` is authored Y-down with lower `zsort` in front; catchlight is Y-up
//! with higher `z_order` in front. A malformed rig is repaired only when the
//! repair cannot change how inochi2d renders it; otherwise the import is
//! refused, since the written `.clm` becomes the source of truth.

pub(crate) mod error;
pub mod inx;
pub mod livliv;
pub(crate) mod read;
pub(crate) mod reflect;
pub(crate) mod schema;
pub(crate) mod to_clm;

#[cfg(test)]
mod import_tests;

pub use error::ImportError;
pub use inx::{parse_inp, InpModel, InpParseError, InxModel, InxParseError, VendorData};
pub use to_clm::{from_inx_model, import_inx_bytes, import_inx_model};
