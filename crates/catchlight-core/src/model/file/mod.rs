//! The file edge: a [`Model`] to and from `.clm` bytes.
//!
//! Writing is total and deterministic. Reading trusts nothing: every check is
//! the file-shaped half of a rule an edit already obeys, so a loaded Model
//! needs no repair pass. A complete model and an addon fragment are disjoint
//! on the wire (a fragment never has a parentless node), and the caller says
//! which it is reading.

mod error;
mod read;
mod replace;
#[cfg(test)]
mod tests;
mod write;

pub use error::{ClmLoadError, LoadError};

use std::collections::{HashMap, HashSet};

use crate::extension::{Payload, Stored, decode_registered};
use crate::formats::clm::ClmPayload;
use crate::formats::clm::{
    self as clm, CheckedExtensions, ClmBinding, ClmComposite, ClmExtension, ClmExtensionBlob,
    ClmExtensionMarker, ClmFile, ClmMask, ClmMeshGroup, ClmNode, ClmNodeKind, ClmParam, ClmPart,
    ClmSlot, ClmSlotPair, ClmSpine, ClmStructure, ClmTexture, ClmTextureRef, ClmWeld,
};
use crate::id::SlotId;
use crate::load_budget::{
    LoadBudget, LoadResource, charge_clm_file, charge_clm_structure, charge_decoded_clm,
};

use super::*;

/// The first deform cell, and its length, whose offsets do not fit a mesh of
/// `expected` offsets.
fn misfit_deform_cell(expected: usize, values: &ClmBindingValues) -> Option<([u32; 2], usize)> {
    values
        .deform_cells()?
        .iter()
        .find(|cell| cell.value.len() != expected)
        .map(|cell| ([cell.x, cell.y], cell.value.len()))
}
