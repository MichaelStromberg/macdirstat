use egui::{Color32, Id, RichText};

use crate::format_size;
use crate::model::color::ColorMap;
use crate::model::tree::{FileNode, TreePath};

const MAX_RENDERED_ITEMS: usize = 2000;

pub fn show(
    ui: &mut egui::Ui,
    root: &FileNode,
    selected: &mut Option<TreePath>,
    color_map: &ColorMap,
) {
    ui.heading("Directory Tree");
    ui.separator();

    // Expand ancestors and scroll only when selection changes (not every frame,
    // otherwise the user can never manually collapse ancestor nodes).
    let last_expanded_id = Id::new("tree_last_expanded");
    let last_expanded: Option<Vec<usize>> = ui.ctx().data_mut(|d| d.get_temp(last_expanded_id));
    let selection_changed = selected.as_ref() != last_expanded.as_ref();
    if selection_changed {
        if let Some(sel_path) = selected.as_ref() {
            expand_to_path(ui.ctx(), sel_path);
        }
        ui.ctx()
            .data_mut(|d| d.insert_temp(last_expanded_id, selected.clone()));
    }

    let mut ctx = TreeCtx {
        selected,
        color_map,
        current_path: Vec::new(),
        rendered: 0,
        visible_paths: Vec::new(),
        selection_changed,
    };

    egui::ScrollArea::vertical()
        .auto_shrink([false; 2])
        .show(ui, |ui| {
            ctx.show_node(ui, root, 0);
        });

    // Handle Up/Down arrow keys for navigation
    if !ctx.visible_paths.is_empty() {
        let arrow = ui.ctx().input(|i| {
            if i.key_pressed(egui::Key::ArrowDown) {
                Some(1i32)
            } else if i.key_pressed(egui::Key::ArrowUp) {
                Some(-1i32)
            } else {
                None
            }
        });

        if let Some(direction) = arrow {
            let selected = &mut ctx.selected;
            if let Some(sel) = selected.as_ref() {
                if let Some(pos) = ctx.visible_paths.iter().position(|p| p == sel) {
                    let new_pos = pos as i32 + direction;
                    if new_pos >= 0 && (new_pos as usize) < ctx.visible_paths.len() {
                        **selected = Some(ctx.visible_paths[new_pos as usize].clone());
                    }
                } else {
                    **selected = Some(ctx.visible_paths[0].clone());
                }
            } else {
                **selected = Some(ctx.visible_paths[0].clone());
            }
        }
    }
}

/// Open all ancestor CollapsingState headers for the given path
/// so the selected node becomes visible in the tree.
fn expand_to_path(ctx: &egui::Context, path: &[usize]) {
    // Open each ancestor prefix (including empty prefix = root)
    for depth in 0..path.len() {
        let prefix = &path[..depth];
        let id = Id::new(("tree", prefix));
        let mut state =
            egui::collapsing_header::CollapsingState::load_with_default_open(ctx, id, false);
        state.set_open(true);
        state.store(ctx);
    }
}

struct TreeCtx<'a> {
    selected: &'a mut Option<Vec<usize>>,
    color_map: &'a ColorMap,
    current_path: Vec<usize>,
    rendered: usize,
    visible_paths: Vec<TreePath>,
    selection_changed: bool,
}

impl<'a> TreeCtx<'a> {
    fn show_node(&mut self, ui: &mut egui::Ui, node: &FileNode, depth: usize) {
        if self.rendered >= MAX_RENDERED_ITEMS {
            return;
        }
        self.rendered += 1;

        let is_selected = self.selected.as_ref() == Some(&self.current_path);

        // Record this path as visible for arrow key navigation
        self.visible_paths.push(self.current_path.clone());

        if node.is_dir && !node.children.is_empty() {
            let id = Id::new(("tree", self.current_path.as_slice()));
            let default_open = depth < 1; // root expanded by default

            let state = egui::collapsing_header::CollapsingState::load_with_default_open(
                ui.ctx(),
                id,
                default_open,
            );

            state
                .show_header(ui, |ui| {
                    let label = format_node_label(node);
                    let text = if is_selected {
                        RichText::new(label).strong().color(Color32::WHITE)
                    } else {
                        RichText::new(label)
                    };
                    let resp = ui.selectable_label(is_selected, text);
                    if resp.clicked() {
                        *self.selected = Some(self.current_path.clone());
                    }
                    if is_selected && self.selection_changed {
                        resp.scroll_to_me(Some(egui::Align::Center));
                    }
                })
                .body(|ui| {
                    let remaining = node.children.len();
                    for (i, child) in node.children.iter().enumerate() {
                        if self.rendered >= MAX_RENDERED_ITEMS {
                            let skipped = remaining - i;
                            ui.label(format!("... and {} more items", skipped));
                            break;
                        }
                        self.current_path.push(i);
                        self.show_node(ui, child, depth + 1);
                        self.current_path.pop();
                    }
                });
        } else {
            let color = if node.is_dir {
                ColorMap::dir_color()
            } else {
                self.color_map.get(node.extension())
            };
            let label = format_node_label(node);
            let text = if is_selected {
                RichText::new(label).strong().color(Color32::WHITE)
            } else {
                RichText::new(label).color(color)
            };

            ui.horizontal(|ui| {
                ui.add_space(4.0);
                let resp = ui.selectable_label(is_selected, text);
                if resp.clicked() {
                    *self.selected = Some(self.current_path.clone());
                }
                if is_selected && self.selection_changed {
                    resp.scroll_to_me(Some(egui::Align::Center));
                }
            });
        }
    }
}

fn format_node_label(node: &FileNode) -> String {
    format!("{} ({})", node.name, format_size(node.size))
}
