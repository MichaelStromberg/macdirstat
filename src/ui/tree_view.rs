use std::cell::Cell;

use egui::{Color32, Id, Rect, RichText, pos2, vec2};

use crate::format_size;
use crate::model::tree::{FileNode, TreePath};

const MAX_RENDERED_ITEMS: usize = 2000;
const SIZE_COL_WIDTH: f32 = 55.0;
const SIZE_COL_MARGIN: f32 = 8.0;
const FADE_WIDTH: f32 = 30.0;

/// macOS Finder-style folder icon colors.
const FOLDER_BODY: Color32 = Color32::from_rgb(86, 182, 249);
const FOLDER_TAB: Color32 = Color32::from_rgb(64, 152, 226);

/// Render the "MacDirStat" branding with "Dir" in blue.
pub fn show_branding(ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.label(RichText::new("Mac").size(16.0).strong());
        ui.label(
            RichText::new("Dir")
                .size(16.0)
                .strong()
                .color(Color32::from_rgb(56, 132, 244)),
        );
        ui.label(RichText::new("Stat").size(16.0).strong());
    });
}

pub fn show(ui: &mut egui::Ui, root: &FileNode, selected: &mut Option<TreePath>) {
    show_branding(ui);
    ui.add_space(4.0);

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

    // Rounded-corner container for the tree (Finder/System Settings style)
    let frame_fill = if ui.visuals().dark_mode {
        Color32::from_rgb(38, 38, 38)
    } else {
        Color32::from_rgb(236, 236, 236)
    };
    let frame = egui::Frame::new()
        .fill(frame_fill)
        .corner_radius(8.0)
        .inner_margin(4.0);

    // Derive alternating row color from the frame's fill
    let alt_row_color = if ui.visuals().dark_mode {
        Color32::from_rgb(
            frame_fill.r().saturating_add(8),
            frame_fill.g().saturating_add(8),
            frame_fill.b().saturating_add(8),
        )
    } else {
        Color32::from_rgb(
            frame_fill.r().saturating_sub(10),
            frame_fill.g().saturating_sub(10),
            frame_fill.b().saturating_sub(10),
        )
    };

    let mut ctx = TreeCtx {
        selected,
        current_path: Vec::new(),
        rendered: 0,
        visible_paths: Vec::new(),
        row_index: 0,
        panel_fill: frame_fill,
        alt_row_color,
        scroll_right: 0.0,
        frame_left: 0.0,
    };

    let available_height = ui.available_height();
    frame.show(ui, |ui| {
        ui.set_min_height(available_height - frame.total_margin().sum().y);
        ctx.frame_left = ui.max_rect().left();
        egui::ScrollArea::vertical()
            .auto_shrink([false; 2])
            .show(ui, |ui| {
                ctx.scroll_right = ui.max_rect().right();
                ctx.show_node(ui, root, 0);
            });
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
    current_path: Vec<usize>,
    rendered: usize,
    visible_paths: Vec<TreePath>,
    row_index: usize,
    panel_fill: Color32,
    alt_row_color: Color32,
    scroll_right: f32,
    frame_left: f32,
}

impl<'a> TreeCtx<'a> {
    /// Paint full-width row background at current cursor position.
    /// Paints every row (panel_fill for even, alt color for odd) to ensure
    /// a uniform background with no gaps between labels and the size column.
    /// When `is_selected`, also paints the blue selection highlight.
    /// Constrained to the frame bounds so backgrounds don't bleed past rounded corners.
    fn paint_row_bg(&mut self, ui: &mut egui::Ui, is_selected: bool) {
        let y = ui.cursor().min.y;
        let bg = if self.row_index % 2 == 1 {
            self.alt_row_color
        } else {
            self.panel_fill
        };
        let width = self.scroll_right - self.frame_left;
        let bg_rect = Rect::from_min_size(pos2(self.frame_left, y), vec2(width, 20.0));
        ui.painter().rect_filled(bg_rect, 0.0, bg);
        if is_selected {
            let sel_color = ui.visuals().selection.bg_fill;
            ui.painter().rect_filled(bg_rect, 0.0, sel_color);
        }
        self.row_index += 1;
    }

    /// Paint the size text at the right edge. Bold+black when selected.
    /// No background overlay — name text fading is handled at render time
    /// via foreground alpha (see `paint_name_faded`).
    fn paint_size(&self, ui: &egui::Ui, y_center: f32, size: u64, is_selected: bool) {
        let size_str = format_size(size);
        let color = if is_selected {
            Color32::BLACK
        } else {
            Color32::from_rgb(140, 140, 140)
        };
        let anchor = pos2(self.scroll_right - SIZE_COL_MARGIN, y_center);
        ui.painter().text(
            anchor,
            egui::Align2::RIGHT_CENTER,
            &size_str,
            egui::FontId::proportional(11.0),
            color,
        );
        if is_selected {
            ui.painter().text(
                anchor + vec2(0.5, 0.0),
                egui::Align2::RIGHT_CENTER,
                &size_str,
                egui::FontId::proportional(11.0),
                color,
            );
        }
    }

    /// Paint the name text with a foreground fade near the size column.
    /// Uses clipped painters to draw text strips with decreasing alpha,
    /// so the text fades out without any background overlay.
    fn paint_name_faded(&self, ui: &egui::Ui, pos: egui::Pos2, name: &str, is_selected: bool) {
        let base_color = if is_selected {
            Color32::WHITE
        } else {
            ui.visuals().text_color()
        };

        let size_area_left = self.scroll_right - SIZE_COL_MARGIN - SIZE_COL_WIDTH;
        let fade_left = size_area_left - FADE_WIDTH;
        let clip = ui.clip_rect();
        let bold_offset = vec2(0.5, 0.0);

        // Helper: paint text (with optional bold double-draw) using given painter and color
        let draw = |painter: &egui::Painter, color: Color32| {
            painter.text(
                pos,
                egui::Align2::LEFT_CENTER,
                name,
                egui::FontId::proportional(14.0),
                color,
            );
            if is_selected {
                painter.text(
                    pos + bold_offset,
                    egui::Align2::LEFT_CENTER,
                    name,
                    egui::FontId::proportional(14.0),
                    color,
                );
            }
        };

        // Full-opacity region: from left edge to start of fade
        let full_clip = Rect::from_min_max(
            pos2(clip.left(), clip.top()),
            pos2(fade_left, clip.bottom()),
        );
        draw(&ui.painter().with_clip_rect(full_clip), base_color);

        // Fade region: multiple strips with decreasing alpha
        let steps: u32 = 8;
        let step_w = FADE_WIDTH / steps as f32;
        for i in 0..steps {
            let t = 1.0 - (i + 1) as f32 / steps as f32;
            let alpha = (t * base_color.a() as f32) as u8;
            let faded = Color32::from_rgba_unmultiplied(
                base_color.r(),
                base_color.g(),
                base_color.b(),
                alpha,
            );
            let strip_left = fade_left + i as f32 * step_w;
            let strip_clip = Rect::from_min_max(
                pos2(strip_left, clip.top()),
                pos2(strip_left + step_w, clip.bottom()),
            );
            draw(&ui.painter().with_clip_rect(strip_clip), faded);
        }
    }

    fn show_node(&mut self, ui: &mut egui::Ui, node: &FileNode, depth: usize) {
        if self.rendered >= MAX_RENDERED_ITEMS {
            return;
        }
        self.rendered += 1;

        let is_selected = self.selected.as_ref() == Some(&self.current_path);

        // Record this path as visible for arrow key navigation
        self.visible_paths.push(self.current_path.clone());

        // Show short name for root node (last path segment), full name otherwise
        let display_name = if depth == 0 {
            node.name.rsplit('/').next().unwrap_or(&node.name)
        } else {
            &node.name
        };

        let y_before = ui.cursor().min.y;

        if node.is_dir && !node.children.is_empty() {
            let id = Id::new(("tree", self.current_path.as_slice()));
            let default_open = depth < 1;

            let state = egui::collapsing_header::CollapsingState::load_with_default_open(
                ui.ctx(),
                id,
                default_open,
            );

            self.paint_row_bg(ui, is_selected);

            let header_row_y = y_before;
            let path_clone = self.current_path.clone();
            let is_sel = is_selected;
            let name_owned = display_name.to_string();
            let name_x = Cell::new(0.0f32);

            state
                .show_header(ui, |ui| {
                    // Allocate space for folder icon and paint it
                    let (icon_rect, _) =
                        ui.allocate_exact_size(vec2(18.0, 14.0), egui::Sense::hover());
                    paint_folder_icon(ui.painter(), icon_rect);

                    // Record x position right after the icon
                    name_x.set(icon_rect.right() + 4.0);

                    // Allocate remaining width as click area
                    let avail = ui.available_size();
                    let (_, resp) = ui.allocate_exact_size(avail, egui::Sense::click());
                    if resp.clicked() {
                        *self.selected = Some(path_clone.clone());
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

            // Paint name with foreground fade and size for header row
            self.paint_name_faded(
                ui,
                pos2(name_x.get(), header_row_y + 10.0),
                &name_owned,
                is_sel,
            );
            self.paint_size(ui, header_row_y + 10.0, node.size, is_sel);
        } else {
            self.paint_row_bg(ui, is_selected);

            let name_x = Cell::new(0.0f32);

            ui.horizontal(|ui| {
                ui.add_space(4.0);

                // Folder icon for empty directories
                if node.is_dir {
                    let (icon_rect, _) =
                        ui.allocate_exact_size(vec2(18.0, 14.0), egui::Sense::hover());
                    paint_folder_icon(ui.painter(), icon_rect);
                    name_x.set(icon_rect.right() + 4.0);
                } else {
                    name_x.set(ui.cursor().min.x);
                }

                // Allocate remaining width as click area
                let avail = ui.available_size();
                let (_, resp) = ui.allocate_exact_size(avail, egui::Sense::click());
                if resp.clicked() {
                    *self.selected = Some(self.current_path.clone());
                }
            });

            // Paint name with foreground fade and size
            self.paint_name_faded(
                ui,
                pos2(name_x.get(), y_before + 10.0),
                display_name,
                is_selected,
            );
            self.paint_size(ui, y_before + 10.0, node.size, is_selected);
        }
    }
}

/// Paint a macOS Finder-style blue folder icon into the given rect.
fn paint_folder_icon(painter: &egui::Painter, rect: Rect) {
    let x = rect.min.x;
    let y = rect.min.y;
    let w = rect.width();
    let h = rect.height();

    // Tab (top-left notch)
    let tab = Rect::from_min_size(pos2(x, y + 0.5), vec2(w * 0.42, h * 0.28));
    painter.rect_filled(tab, 1.5, FOLDER_TAB);

    // Body (main folder rectangle)
    let body = Rect::from_min_size(pos2(x, y + h * 0.22), vec2(w, h * 0.78));
    painter.rect_filled(body, 2.0, FOLDER_BODY);
}
