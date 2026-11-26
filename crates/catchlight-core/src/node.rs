use indextree::{Arena, NodeEdge, NodeId as TreeNodeId};
use std::collections::HashMap;
use std::sync::OnceLock;

use crate::components::NodeIdx;

#[derive(Debug, Clone)]
pub struct NodeTree {
    pub root: NodeIdx,
    arena: Arena<NodeIdx>,
    node_to_tree: HashMap<NodeIdx, TreeNodeId>,
    // Dense parent lookup indexed by NodeIdx slot, maintained by add_child.
    // Avoids a hash probe on the per-node transform walk.
    parent: Vec<Option<NodeIdx>>,
    // Pre-order DFS cache, invalidated by add_child.
    dfs_cache: OnceLock<Box<[NodeIdx]>>,
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
            dfs_cache: OnceLock::new(),
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
        self.dfs_cache = OnceLock::new();

        Ok(())
    }

    pub fn children(&self, node: NodeIdx) -> impl Iterator<Item = NodeIdx> + '_ {
        self.node_to_tree
            .get(&node)
            .into_iter()
            .flat_map(|&tree_node| tree_node.children(&self.arena))
            .map(|id| *self.arena[id].get())
    }

    pub fn get_children(&self, node: NodeIdx) -> Vec<NodeIdx> {
        self.children(node).collect()
    }

    pub fn get_parent(&self, node: NodeIdx) -> Option<NodeIdx> {
        self.parent.get(node.0 as usize).copied().flatten()
    }

    pub fn get_all_descendants(&self, node: NodeIdx) -> Vec<NodeIdx> {
        let Some(&tree_node) = self.node_to_tree.get(&node) else {
            return Vec::new();
        };
        // descendants() includes the root itself.
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

    pub fn dfs_order(&self) -> &[NodeIdx] {
        self.dfs_cache.get_or_init(|| {
            let mut order = Vec::new();
            self.traverse_depth_first(|id| order.push(id));
            order.into()
        })
    }
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
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

        assert_eq!(tree.dfs_order().len(), 2);

        tree.add_child(root, NodeIdx::new(2)).unwrap();
        assert_eq!(tree.dfs_order().len(), 3);
    }

    #[test]
    fn children_of_a_missing_node_are_empty() {
        let tree = NodeTree::new(NodeIdx::new(0));
        assert_eq!(tree.children(NodeIdx::new(7)).count(), 0);
    }
}
