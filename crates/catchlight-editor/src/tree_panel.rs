//! Node tree panel: selection, filter, drag-and-drop reparent/reorder, and the
//! per-node context menu. Pure UI — it emits [`TreeAction`]s the app applies
//! through the command dispatch, so the tree can never mutate state itself.

use std::collections::HashSet;

use catchlight_editor_protocol::{NodeKindArg, NodeRef, TreeNode};
use eframe::egui;

pub(crate) enum TreeAction {
    Select {
        node: NodeRef,
        additive: bool,
        range: bool,
    },
    /// Reparent `node` under `parent` (appended), in one undo step.
    DropInto {
        node: NodeRef,
        parent: NodeRef,
    },
    /// Insert `node` at `index` among `parent`'s children, in one undo step.
    DropBefore {
        node: NodeRef,
        parent: NodeRef,
        index: u32,
    },
    AddChild {
        parent: NodeRef,
        kind: NodeKindArg,
    },
    AddPhysics {
        parent: NodeRef,
    },
    Duplicate(NodeRef),
    Delete(NodeRef),
    SetEnabled {
        node: NodeRef,
        enabled: bool,
    },
    Isolate(Option<NodeRef>),
    Focus(NodeRef),
}

pub(crate) struct TreePanel<'a> {
    pub selection: &'a HashSet<u64>,
    pub isolated: Option<NodeRef>,
    pub filter: &'a str,
    pub collapsed: &'a mut HashSet<u64>,
    pub actions: Vec<TreeAction>,
    /// Rows in display order — the range-select universe.
    pub visible: Vec<NodeRef>,
}

impl TreePanel<'_> {
    pub(crate) fn show(&mut self, ui: &mut egui::Ui, root: &TreeNode) {
        let filtering = !self.filter.is_empty();
        self.node_row(ui, root, None, 0, 0, filtering);
    }

    /// Does this subtree contain a filter match?
    fn matches(&self, node: &TreeNode) -> bool {
        if node
            .name
            .to_lowercase()
            .contains(&self.filter.to_lowercase())
        {
            return true;
        }
        node.children.iter().any(|c| self.matches(c))
    }

    #[allow(clippy::too_many_arguments)]
    fn node_row(
        &mut self,
        ui: &mut egui::Ui,
        node: &TreeNode,
        parent: Option<NodeRef>,
        index_in_parent: usize,
        depth: usize,
        filtering: bool,
    ) {
        if filtering && !self.matches(node) {
            return;
        }
        let collapsed = !filtering && self.collapsed.contains(&node.node.0);
        self.visible.push(node.node);

        let row = ui
            .horizontal(|ui| {
                ui.add_space(depth as f32 * 12.0);
                if node.children.is_empty() {
                    ui.add_space(14.0);
                } else {
                    let tri = if collapsed { "▶" } else { "▼" };
                    if ui
                        .add(egui::Button::new(tri).small().frame(false))
                        .clicked()
                    {
                        if collapsed {
                            self.collapsed.remove(&node.node.0);
                        } else {
                            self.collapsed.insert(node.node.0);
                        }
                    }
                }
                let eye = if node.enabled { "👁" } else { "🚫" };
                if ui
                    .add(egui::Button::new(eye).small().frame(false))
                    .on_hover_text("hide / show subtree")
                    .clicked()
                {
                    self.actions.push(TreeAction::SetEnabled {
                        node: node.node,
                        enabled: !node.enabled,
                    });
                }

                let selected = self.selection.contains(&node.node.0);
                let isolated_here = self.isolated == Some(node.node);
                let label = format!(
                    "{}{} · {}",
                    if isolated_here { "🔍 " } else { "" },
                    node.name,
                    node.kind
                );
                let is_root = parent.is_none();
                // A plain selectable label that only *becomes* a drag source
                // once an actual drag starts: wrapping rows in
                // `dnd_drag_source` gives the drag sense the whole row, so
                // hovering shows the grab cursor and most clicks are eaten by
                // the drag arbitration instead of selecting the node.
                let label_resp = ui
                    .selectable_label(selected, label)
                    .interact(egui::Sense::click_and_drag());
                if !is_root {
                    label_resp.dnd_set_drag_payload(node.node);
                    if label_resp.dragged() {
                        egui::Area::new(egui::Id::new(("tree-drag-ghost", node.node.0)))
                            .fixed_pos(
                                ui.ctx()
                                    .pointer_interact_pos()
                                    .unwrap_or(label_resp.rect.left_top())
                                    + egui::vec2(12.0, -6.0),
                            )
                            .order(egui::Order::Tooltip)
                            .show(ui.ctx(), |ui| {
                                ui.label(&node.name);
                            });
                    }
                }

                if label_resp.clicked() {
                    let mods = ui.input(|i| i.modifiers);
                    self.actions.push(TreeAction::Select {
                        node: node.node,
                        additive: mods.ctrl || mods.command,
                        range: mods.shift,
                    });
                }
                self.context_menu(&label_resp, node, is_root);
                if !is_root {
                    self.drop_target(ui, &label_resp, node, parent, index_in_parent);
                } else if let Some(payload) = label_resp.dnd_release_payload::<NodeRef>() {
                    if *payload != node.node {
                        self.actions.push(TreeAction::DropInto {
                            node: *payload,
                            parent: node.node,
                        });
                    }
                }
                label_resp
            })
            .inner;
        let _ = row;

        if !collapsed {
            for (i, child) in node.children.iter().enumerate() {
                self.node_row(ui, child, Some(node.node), i, depth + 1, filtering);
            }
        }
    }

    fn drop_target(
        &mut self,
        ui: &egui::Ui,
        resp: &egui::Response,
        node: &TreeNode,
        parent: Option<NodeRef>,
        index_in_parent: usize,
    ) {
        let (Some(parent), Some(pos)) = (parent, ui.ctx().pointer_hover_pos()) else {
            return;
        };
        let rect = resp.rect;
        let zone = if pos.y < rect.top() + rect.height() * 0.25 {
            DropZone::Before
        } else if pos.y > rect.bottom() - rect.height() * 0.25 {
            DropZone::After
        } else {
            DropZone::Into
        };

        if let Some(payload) = resp.dnd_hover_payload::<NodeRef>() {
            if *payload != node.node {
                let paint = ui.painter();
                let stroke = egui::Stroke::new(2.0_f32, ui.visuals().selection.bg_fill);
                match zone {
                    DropZone::Before => {
                        paint.hline(rect.x_range(), rect.top(), stroke);
                    }
                    DropZone::After => {
                        paint.hline(rect.x_range(), rect.bottom(), stroke);
                    }
                    DropZone::Into => {
                        paint.rect_stroke(rect, 2.0, stroke, egui::StrokeKind::Outside);
                    }
                }
            }
        }
        if let Some(payload) = resp.dnd_release_payload::<NodeRef>() {
            if *payload == node.node {
                return;
            }
            match zone {
                DropZone::Into => self.actions.push(TreeAction::DropInto {
                    node: *payload,
                    parent: node.node,
                }),
                DropZone::Before => self.actions.push(TreeAction::DropBefore {
                    node: *payload,
                    parent,
                    index: index_in_parent as u32,
                }),
                DropZone::After => self.actions.push(TreeAction::DropBefore {
                    node: *payload,
                    parent,
                    index: index_in_parent as u32 + 1,
                }),
            }
        }
    }

    fn context_menu(&mut self, resp: &egui::Response, node: &TreeNode, is_root: bool) {
        resp.context_menu(|ui| {
            ui.menu_button("Add child", |ui| {
                for (label, kind) in [
                    ("Empty", NodeKindArg::Empty),
                    ("Part", NodeKindArg::Part),
                    ("Composite", NodeKindArg::Composite),
                    ("MeshGroup", NodeKindArg::MeshGroup),
                ] {
                    if ui.button(label).clicked() {
                        self.actions.push(TreeAction::AddChild {
                            parent: node.node,
                            kind,
                        });
                        ui.close();
                    }
                }
                if ui.button("SimplePhysics").clicked() {
                    self.actions
                        .push(TreeAction::AddPhysics { parent: node.node });
                    ui.close();
                }
            });
            if !is_root {
                if ui.button("Duplicate").clicked() {
                    self.actions.push(TreeAction::Duplicate(node.node));
                    ui.close();
                }
                if ui.button("Delete").clicked() {
                    self.actions.push(TreeAction::Delete(node.node));
                    ui.close();
                }
            }
            ui.separator();
            if self.isolated == Some(node.node) {
                if ui.button("Show all (un-isolate)").clicked() {
                    self.actions.push(TreeAction::Isolate(None));
                    ui.close();
                }
            } else if ui.button("Isolate subtree").clicked() {
                self.actions.push(TreeAction::Isolate(Some(node.node)));
                ui.close();
            }
            if ui.button("Focus in viewport").clicked() {
                self.actions.push(TreeAction::Focus(node.node));
                ui.close();
            }
        });
    }
}

enum DropZone {
    Before,
    Into,
    After,
}
