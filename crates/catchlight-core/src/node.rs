use crate::components::NodeIdx;

/// A puppet's node tree, numbered in pre-order from the root at 0: the
/// subtree under a node is the index range from it to its subtree end, so
/// walking a subtree is a loop over indices.
#[derive(Debug, Clone)]
pub struct NodeTree {
    parent: Vec<Option<NodeIdx>>,
    /// Exclusive; indexed like `parent`.
    subtree_end: Vec<u32>,
}

impl Default for NodeTree {
    fn default() -> Self {
        Self::new()
    }
}

impl NodeTree {
    /// A tree holding only the root.
    pub fn new() -> Self {
        Self {
            parent: vec![None],
            subtree_end: vec![1],
        }
    }

    pub fn root(&self) -> NodeIdx {
        NodeIdx(0)
    }

    pub(crate) fn len(&self) -> usize {
        self.parent.len()
    }

    /// Appends a node under `parent` and returns its index. To keep the
    /// numbering pre-order, `parent` must be the last node or one of its
    /// ancestors.
    pub(crate) fn push_child(&mut self, parent: NodeIdx) -> Result<NodeIdx, NodeTreeError> {
        let next = self.parent.len() as u32;
        match self.subtree_end.get(parent.0 as usize) {
            None => Err(NodeTreeError::NodeNotFound(parent)),
            Some(&end) if end != next => Err(NodeTreeError::OutOfOrder(parent)),
            Some(_) => Ok(self.append(parent)),
        }
    }

    /// Appends a node under the root, which is always an ancestor of the last.
    pub(crate) fn push_root_child(&mut self) -> NodeIdx {
        self.append(self.root())
    }

    fn append(&mut self, parent: NodeIdx) -> NodeIdx {
        let child = self.parent.len() as u32;
        self.parent.push(Some(parent));
        self.subtree_end.push(child + 1);
        let mut above = Some(parent);
        while let Some(node) = above {
            self.subtree_end[node.0 as usize] = child + 1;
            above = self.parent[node.0 as usize];
        }
        NodeIdx(child)
    }

    /// The index after the last node under `node`; 0 for a missing node.
    pub(crate) fn subtree_end(&self, node: NodeIdx) -> NodeIdx {
        NodeIdx(self.subtree_end.get(node.0 as usize).copied().unwrap_or(0))
    }

    pub fn children(&self, node: NodeIdx) -> impl Iterator<Item = NodeIdx> + '_ {
        let end = self.subtree_end(node).0;
        let mut next = node.0.saturating_add(1);
        std::iter::from_fn(move || {
            let child = next;
            if child >= end {
                return None;
            }
            next = self.subtree_end[child as usize];
            Some(NodeIdx(child))
        })
    }

    pub fn get_children(&self, node: NodeIdx) -> Vec<NodeIdx> {
        self.children(node).collect()
    }

    pub fn get_parent(&self, node: NodeIdx) -> Option<NodeIdx> {
        self.parent.get(node.0 as usize).copied().flatten()
    }

    /// Every node under `node`, in pre-order, `node` excluded.
    pub fn descendants(&self, node: NodeIdx) -> impl Iterator<Item = NodeIdx> + use<> {
        (node.0.saturating_add(1)..self.subtree_end(node).0).map(NodeIdx)
    }

    /// Pre-order, so a parent is visited before its children.
    pub fn traverse_depth_first<F>(&self, visitor: F)
    where
        F: FnMut(NodeIdx),
    {
        (0..self.len() as u32).map(NodeIdx).for_each(visitor);
    }
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum NodeTreeError {
    #[error("Node {0:?} not found in tree")]
    NodeNotFound(NodeIdx),
    #[error("node {0:?} is neither the last node nor one of its ancestors")]
    OutOfOrder(NodeIdx),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// root(0) -> a(1) -> a1(2), a2(3); root -> b(4) -> b1(5)
    fn sample() -> NodeTree {
        let mut tree = NodeTree::new();
        let root = tree.root();
        let a = tree.push_child(root).unwrap();
        tree.push_child(a).unwrap();
        tree.push_child(a).unwrap();
        let b = tree.push_child(root).unwrap();
        tree.push_child(b).unwrap();
        tree
    }

    fn idx(ids: &[u32]) -> Vec<NodeIdx> {
        ids.iter().copied().map(NodeIdx).collect()
    }

    #[test]
    fn a_new_tree_is_its_root() {
        let tree = NodeTree::new();
        assert_eq!(tree.root(), NodeIdx(0));
        assert_eq!(tree.get_parent(tree.root()), None);
        assert_eq!(tree.children(tree.root()).count(), 0);
        assert_eq!(tree.descendants(tree.root()).count(), 0);
    }

    #[test]
    fn nodes_are_numbered_in_pre_order() {
        let tree = sample();
        assert_eq!(tree.get_children(NodeIdx(0)), idx(&[1, 4]));
        assert_eq!(tree.get_children(NodeIdx(1)), idx(&[2, 3]));
        assert_eq!(tree.get_children(NodeIdx(3)), idx(&[]));
        assert_eq!(tree.get_parent(NodeIdx(3)), Some(NodeIdx(1)));
        assert_eq!(tree.get_parent(NodeIdx(5)), Some(NodeIdx(4)));
        assert_eq!(
            tree.descendants(NodeIdx(0)).collect::<Vec<_>>(),
            idx(&[1, 2, 3, 4, 5])
        );
        assert_eq!(
            tree.descendants(NodeIdx(1)).collect::<Vec<_>>(),
            idx(&[2, 3])
        );
        let mut visited = Vec::new();
        tree.traverse_depth_first(|node| visited.push(node));
        assert_eq!(visited, idx(&[0, 1, 2, 3, 4, 5]));
    }

    #[test]
    fn a_child_follows_the_last_node_or_one_of_its_ancestors() {
        let mut tree = sample();
        assert!(matches!(
            tree.push_child(NodeIdx(1)),
            Err(NodeTreeError::OutOfOrder(NodeIdx(1)))
        ));
        assert!(matches!(
            tree.push_child(NodeIdx(9)),
            Err(NodeTreeError::NodeNotFound(NodeIdx(9)))
        ));
        assert_eq!(tree.push_child(NodeIdx(5)).unwrap(), NodeIdx(6));
        assert_eq!(tree.push_root_child(), NodeIdx(7));
        assert_eq!(tree.subtree_end(NodeIdx(4)), NodeIdx(7));
        assert_eq!(tree.subtree_end(NodeIdx(0)), NodeIdx(8));
    }

    #[test]
    fn a_missing_node_has_no_children_or_descendants() {
        let tree = NodeTree::new();
        assert_eq!(tree.children(NodeIdx(7)).count(), 0);
        assert_eq!(tree.descendants(NodeIdx(7)).count(), 0);
        assert_eq!(tree.descendants(NodeIdx(u32::MAX)).count(), 0);
        assert_eq!(tree.get_parent(NodeIdx(7)), None);
    }
}
