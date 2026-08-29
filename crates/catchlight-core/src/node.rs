use indextree::{Arena, NodeEdge, NodeId as TreeNodeId};
use std::collections::HashMap;
use std::sync::Mutex;

use crate::components::NodeIdx;

#[derive(Debug)]
pub struct NodeTree {
    pub root: NodeIdx,
    arena: Arena<NodeIdx>,
    node_to_tree: HashMap<NodeIdx, TreeNodeId>,
    // Dense parent lookup indexed by NodeIdx slot, maintained by
    // add_child (the tree's only mutator). get_parent is hit once per
    // node per transform walk, so it must not pay the node_to_tree
    // hash probe + arena hop.
    parent: Vec<Option<NodeIdx>>,
    // Cached pre-order DFS of all nodes. Invalidated on structural changes
    // (add_child). Populated lazily on the first dfs_order() call. Mutex
    // rather than RefCell so the whole tree is Sync (required by bevy
    // Components).
    dfs_cache: Mutex<Option<Vec<NodeIdx>>>,
}

impl Clone for NodeTree {
    fn clone(&self) -> Self {
        Self {
            root: self.root,
            arena: self.arena.clone(),
            node_to_tree: self.node_to_tree.clone(),
            parent: self.parent.clone(),
            // The cache can always be rebuilt from the arena; don't bother
            // cloning it through the Mutex.
            dfs_cache: Mutex::new(None),
        }
    }
}

impl NodeTree {
    pub fn new(root: NodeIdx) -> Self {
        let mut arena = Arena::new();
        let root_tree = arena.new_node(root);

        let mut node_to_tree = HashMap::new();
        node_to_tree.insert(root, root_tree);

        Self {
            root,
            arena,
            node_to_tree,
            parent: Vec::new(),
            dfs_cache: Mutex::new(None),
        }
    }

    fn set_parent_slot(&mut self, child: NodeIdx, parent: NodeIdx) {
        let slot = child.0 as usize;
        if self.parent.len() <= slot {
            self.parent.resize(slot + 1, None);
        }
        self.parent[slot] = Some(parent);
    }

    pub fn add_child(&mut self, parent: NodeIdx, child: NodeIdx) -> Result<(), NodeTreeError> {
        let parent_tree = self
            .node_to_tree
            .get(&parent)
            .ok_or(NodeTreeError::NodeNotFound(parent))?;

        let child_tree = self.arena.new_node(child);
        parent_tree.append(child_tree, &mut self.arena);
        self.node_to_tree.insert(child, child_tree);
        self.set_parent_slot(child, parent);
        *self.dfs_cache.get_mut().unwrap_or_else(|e| e.into_inner()) = None;

        Ok(())
    }

    // Invariant: indextree ids returned by children() all live in self.arena.
    #[allow(clippy::unwrap_used)]
    pub fn get_children(&self, node: NodeIdx) -> Vec<NodeIdx> {
        if let Some(&tree_node) = self.node_to_tree.get(&node) {
            tree_node
                .children(&self.arena)
                .map(|child_id| *self.arena.get(child_id).unwrap().get())
                .collect()
        } else {
            Vec::new()
        }
    }

    pub fn get_parent(&self, node: NodeIdx) -> Option<NodeIdx> {
        self.parent.get(node.0 as usize).copied().flatten()
    }

    pub fn get_all_descendants(&self, node: NodeIdx) -> Vec<NodeIdx> {
        let Some(&tree_node) = self.node_to_tree.get(&node) else {
            return Vec::new();
        };
        // indextree::descendants() is iterative and includes the root itself,
        // so skip it to match the "descendants of `node`" contract.
        tree_node
            .descendants(&self.arena)
            .skip(1)
            .map(|id| *self.arena[id].get())
            .collect()
    }

    pub fn get_descendants_until<F>(&self, node: NodeIdx, stop_condition: F) -> Vec<NodeIdx>
    where
        F: Fn(NodeIdx) -> bool,
    {
        let Some(&tree_node) = self.node_to_tree.get(&node) else {
            return Vec::new();
        };

        let mut descendants = Vec::new();
        let mut iter = tree_node.traverse(&self.arena);
        // Consume the root's Start event so we don't record the node itself.
        iter.next();

        let mut skipping_under: Option<TreeNodeId> = None;
        for edge in iter {
            match edge {
                NodeEdge::Start(tid) => {
                    if skipping_under.is_some() {
                        continue;
                    }
                    let child = *self.arena[tid].get();
                    descendants.push(child);
                    if stop_condition(child) {
                        skipping_under = Some(tid);
                    }
                }
                NodeEdge::End(tid) => {
                    if skipping_under == Some(tid) {
                        skipping_under = None;
                    }
                }
            }
        }
        descendants
    }

    pub fn traverse_depth_first<F>(&self, mut visitor: F)
    where
        F: FnMut(NodeIdx),
    {
        let Some(&root_tree) = self.node_to_tree.get(&self.root) else {
            return;
        };
        for id in root_tree.descendants(&self.arena) {
            visitor(*self.arena[id].get());
        }
    }

    /// Run `f` with a borrowed slice of the pre-order DFS, cached until
    /// the tree mutates. A callback (rather than returning an owned
    /// Vec) keeps hot-path callers allocation-free.
    // `f` runs while the guard is held, so a panic in it would poison the
    // mutex. Poison carries no information here: the cache is a pure function
    // of the tree and is only ever written whole, so both accessors recover the
    // inner value instead of propagating. Without this a single panicking `f`
    // would wedge every later traversal of this tree.
    pub fn with_dfs_order<R>(&self, f: impl FnOnce(&[NodeIdx]) -> R) -> R {
        let mut guard = self.dfs_cache.lock().unwrap_or_else(|e| e.into_inner());
        let order = guard.get_or_insert_with(|| {
            let mut order = Vec::new();
            self.traverse_depth_first(|id| order.push(id));
            order
        });
        f(order)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum NodeTreeError {
    #[error("Node {0:?} not found in tree")]
    NodeNotFound(NodeIdx),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_node_tree() {
        let root = NodeIdx::new(0);
        let tree = NodeTree::new(root);
        assert_eq!(tree.root, root);
    }

    #[test]
    fn add_child_to_tree() {
        let root = NodeIdx::new(0);
        let mut tree = NodeTree::new(root);

        let child = NodeIdx::new(1);
        let result = tree.add_child(root, child);

        assert!(result.is_ok());

        let children = tree.get_children(root);
        assert_eq!(children.len(), 1);
        assert_eq!(children[0], child);
    }

    #[test]
    fn get_parent_from_tree() {
        let root = NodeIdx::new(0);
        let mut tree = NodeTree::new(root);

        let child = NodeIdx::new(1);
        tree.add_child(root, child).unwrap();

        let parent = tree.get_parent(child);
        assert_eq!(parent, Some(root));
    }

    #[test]
    fn traverse_tree_depth_first() {
        let root = NodeIdx::new(0);
        let mut tree = NodeTree::new(root);

        let child1 = NodeIdx::new(1);
        let child2 = NodeIdx::new(2);
        let grandchild = NodeIdx::new(3);

        tree.add_child(root, child1).unwrap();
        tree.add_child(root, child2).unwrap();
        tree.add_child(child1, grandchild).unwrap();

        let mut visited = Vec::new();
        tree.traverse_depth_first(|node| {
            visited.push(node);
        });

        assert_eq!(visited.len(), 4);
        assert_eq!(visited[0], root);
        assert!(visited.contains(&child1));
        assert!(visited.contains(&child2));
        assert!(visited.contains(&grandchild));
    }

    #[test]
    fn dfs_order_invalidates_on_add_child() {
        let root = NodeIdx::new(0);
        let mut tree = NodeTree::new(root);
        tree.add_child(root, NodeIdx::new(1)).unwrap();

        // Prime the cache.
        let first_len = tree.with_dfs_order(|o| o.len());
        assert_eq!(first_len, 2);

        // Add a child after priming — cache must reflect the new node.
        tree.add_child(root, NodeIdx::new(2)).unwrap();
        assert_eq!(tree.with_dfs_order(|o| o.len()), 3);
    }

    #[test]
    fn panicking_visitor_does_not_wedge_later_traversals() {
        let root = NodeIdx::new(0);
        let mut tree = NodeTree::new(root);
        tree.add_child(root, NodeIdx::new(1)).unwrap();

        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            tree.with_dfs_order(|_| panic!("visitor blew up"));
        }));
        std::panic::set_hook(prev);
        assert!(caught.is_err());

        // The guard was live across the panic, so the mutex is poisoned.
        // Reads and cache invalidation must both still work.
        assert_eq!(tree.with_dfs_order(|o| o.len()), 2);
        tree.add_child(root, NodeIdx::new(2)).unwrap();
        assert_eq!(tree.with_dfs_order(|o| o.len()), 3);
    }
}
