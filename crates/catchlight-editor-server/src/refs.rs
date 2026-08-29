//! Protocol handles <-> model Ids.
//!
//! The wire still carries opaque `u64` handles ([`NodeRef`], [`ParamRef`],
//! [`TexRef`]); a [`Model`](catchlight_core::Model) is keyed by string Ids.
//! Each session interns the Ids it has handed out, so one Id always gets the
//! same handle back and a handle stays valid for the session's lifetime —
//! across undo, redo and a reopened document — because it names an Id, not a
//! slot. cl-32i.12 puts Ids on the wire and deletes this module.

use std::collections::HashMap;
use std::hash::Hash;

use catchlight_core::id::{NodeId, ParamId, TexId};
use catchlight_editor_protocol::{NodeRef, ParamRef, TexRef};

/// Handles are 1-based so that 0 is never a live handle.
struct Table<T> {
    ids: Vec<T>,
    handles: HashMap<T, u64>,
}

impl<T> Default for Table<T> {
    fn default() -> Self {
        Self {
            ids: Vec::new(),
            handles: HashMap::new(),
        }
    }
}

impl<T: Clone + Eq + Hash> Table<T> {
    fn intern(&mut self, id: &T) -> u64 {
        if let Some(&handle) = self.handles.get(id) {
            return handle;
        }
        self.ids.push(id.clone());
        let handle = self.ids.len() as u64;
        self.handles.insert(id.clone(), handle);
        handle
    }

    fn id(&self, handle: u64) -> Option<&T> {
        self.ids.get(usize::try_from(handle.checked_sub(1)?).ok()?)
    }
}

#[derive(Default)]
pub struct RefMap {
    nodes: Table<NodeId>,
    params: Table<ParamId>,
    textures: Table<TexId>,
}

impl std::fmt::Debug for RefMap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RefMap")
            .field("nodes", &self.nodes.ids.len())
            .field("params", &self.params.ids.len())
            .field("textures", &self.textures.ids.len())
            .finish()
    }
}

impl RefMap {
    pub fn node(&mut self, id: &NodeId) -> NodeRef {
        NodeRef(self.nodes.intern(id))
    }

    pub fn param(&mut self, id: &ParamId) -> ParamRef {
        ParamRef(self.params.intern(id))
    }

    pub fn texture(&mut self, id: &TexId) -> TexRef {
        TexRef(self.textures.intern(id))
    }

    pub fn node_id(&self, handle: NodeRef) -> Option<&NodeId> {
        self.nodes.id(handle.0)
    }

    pub fn param_id(&self, handle: ParamRef) -> Option<&ParamId> {
        self.params.id(handle.0)
    }

    pub fn tex_id(&self, handle: TexRef) -> Option<&TexId> {
        self.textures.id(handle.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_id_always_gets_one_handle_back() {
        let mut refs = RefMap::default();
        let a = NodeId::new("root/part-00000001").unwrap();
        let b = NodeId::new("root/part-00000002").unwrap();
        let ha = refs.node(&a);
        assert_eq!(refs.node(&a), ha, "interning is idempotent");
        assert_ne!(refs.node(&b), ha);
        assert_eq!(refs.node_id(ha), Some(&a));
        assert_eq!(refs.node_id(NodeRef(0)), None);
        assert_eq!(refs.node_id(NodeRef(99)), None);
    }
}
