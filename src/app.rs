use std::path::PathBuf;
use std::time::Instant;

use eframe::egui;

use crate::format_size;
use crate::model::color::ColorMap;
use crate::model::tree::FileTree;
use crate::ui;

pub struct App {
    state: AppState,
}

enum AppState {
    Empty,
    Scanning { path: PathBuf, start_time: Instant },
    Loaded(Box<LoadedState>),
}

struct LoadedState {
    tree: FileTree,
    color_map: ColorMap,
    selected: Option<Vec<usize>>,
    scan_time_ms: f64,
    cached_layout_size: Option<(f32, f32)>,
    treemap_texture: Option<egui::TextureHandle>,
    pending_delete: Option<PendingDelete>,
}

struct PendingDelete {
    /// Index path to the node in the tree.
    path: Vec<usize>,
    /// Full filesystem path for display and deletion.
    fs_path: PathBuf,
    /// Display name.
    name: String,
    /// Size in bytes.
    size: u64,
    /// Whether this is a directory.
    is_dir: bool,
}

impl App {
    pub fn new(_cc: &eframe::CreationContext<'_>, initial_path: Option<String>) -> Self {
        let mut app = Self {
            state: AppState::Empty,
        };
        if let Some(path) = initial_path {
            app.start_scan(PathBuf::from(path));
        }
        app
    }

    fn start_scan(&mut self, path: PathBuf) {
        self.state = AppState::Scanning {
            path,
            start_time: Instant::now(),
        };
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Scanning is synchronous — blocks the UI thread during scan
        if let AppState::Scanning { path, start_time } = &self.state {
            let path = path.clone();
            let start = *start_time;
            let tree = FileTree::scan(&path);
            let color_map = ColorMap::from_extensions(&tree.extensions);
            let scan_time_ms = start.elapsed().as_secs_f64() * 1000.0;
            self.state = AppState::Loaded(Box::new(LoadedState {
                tree,
                color_map,
                selected: None,
                scan_time_ms,
                cached_layout_size: None,
                treemap_texture: None,
                pending_delete: None,
            }));
        }

        egui::TopBottomPanel::top("menu_bar").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("Open Folder...").clicked() {
                        if let Some(path) = rfd_pick_folder() {
                            self.start_scan(path);
                        }
                        ui.close_menu();
                    }
                    if ui.button("Scan Home").clicked() {
                        if let Ok(home) = std::env::var("HOME") {
                            self.start_scan(PathBuf::from(home));
                        }
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("Quit").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
            });
        });

        match &mut self.state {
            AppState::Empty => {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.centered_and_justified(|ui| {
                        ui.vertical_centered(|ui| {
                            ui.add_space(100.0);
                            ui.heading("MacDirStat");
                            ui.add_space(20.0);
                            ui.label("Select a folder to scan from the File menu,");
                            ui.label("or drop a folder onto this window.");
                            ui.add_space(20.0);
                            if ui.button("Scan Home Directory").clicked()
                                && let Ok(home) = std::env::var("HOME")
                            {
                                self.start_scan(PathBuf::from(home));
                            }
                        });
                    });
                });
            }
            AppState::Scanning { .. } => {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.centered_and_justified(|ui| {
                        ui.heading("Scanning...");
                    });
                });
            }
            AppState::Loaded(loaded) => {
                let LoadedState {
                    tree,
                    color_map,
                    selected,
                    scan_time_ms,
                    cached_layout_size,
                    treemap_texture,
                    pending_delete,
                } = loaded.as_mut();
                // Handle Delete key press when something is selected and no dialog open
                if let (None, Some(sel_path)) = (&*pending_delete, selected.as_ref()) {
                    let delete_pressed = ctx.input(|i| {
                        i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace)
                    });
                    if let (true, Some(fs_path), Some(node)) = (
                        delete_pressed,
                        tree.build_fs_path(sel_path),
                        resolve_selected(&tree.root, sel_path),
                    ) {
                        *pending_delete = Some(PendingDelete {
                            path: sel_path.clone(),
                            fs_path,
                            name: node.name.to_string(),
                            size: node.size,
                            is_dir: node.is_dir,
                        });
                    }
                }

                // Show delete confirmation dialog
                let mut delete_action: Option<DeleteAction> = None;
                if let Some(pd) = pending_delete.as_ref() {
                    delete_action = show_delete_dialog(ctx, pd);
                }
                match delete_action {
                    Some(DeleteAction::Confirm) => {
                        let pd = pending_delete.take().unwrap();
                        let result = if pd.is_dir {
                            std::fs::remove_dir_all(&pd.fs_path)
                        } else {
                            std::fs::remove_file(&pd.fs_path)
                        };
                        match result {
                            Ok(()) => {
                                let size = pd.size;
                                let file_count;
                                let dir_count;
                                if let Some(node) = resolve_selected(&tree.root, &pd.path) {
                                    file_count = node.file_count;
                                    dir_count = node.dir_count;
                                } else {
                                    file_count = 0;
                                    dir_count = 0;
                                }
                                tree.subtract_from_ancestors(&pd.path, size, file_count, dir_count);
                                tree.remove_at_path(&pd.path);
                                tree.rebuild_extensions();
                                *color_map = ColorMap::from_extensions(&tree.extensions);

                                // Auto-select next sibling, previous sibling, or parent
                                *selected = next_selection_after_delete(&tree.root, &pd.path);

                                *cached_layout_size = None;
                                *treemap_texture = None;
                            }
                            Err(e) => {
                                eprintln!("Failed to delete {:?}: {}", pd.fs_path, e);
                            }
                        }
                    }
                    Some(DeleteAction::Cancel) => {
                        *pending_delete = None;
                    }
                    None => {}
                }

                // Status bar
                egui::TopBottomPanel::bottom("status_bar").show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(format!(
                            "{} | {} files | {} dirs | {} | Scanned in {:.0}ms",
                            tree.root_path,
                            tree.root.file_count,
                            tree.root.dir_count,
                            format_size(tree.root.size),
                            scan_time_ms,
                        ));
                    });
                });

                // Right panel: extension statistics
                egui::SidePanel::right("extensions")
                    .default_width(220.0)
                    .show(ctx, |ui| {
                        ui::extensions::show(ui, &tree.extensions, color_map);
                    });

                // Left panel: tree view
                egui::SidePanel::left("tree_view")
                    .default_width(350.0)
                    .show(ctx, |ui| {
                        ui::tree_view::show(ui, &tree.root, selected, color_map);
                    });

                // Central panel: treemap
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui::treemap_view::show(
                        ui,
                        tree,
                        selected,
                        color_map,
                        cached_layout_size,
                        treemap_texture,
                    );
                });
            }
        }
    }
}

/// After deleting the node at `deleted_path`, determine what to select next:
/// 1. Same index (next sibling shifted into place) if it exists
/// 2. Previous sibling if deleted was last child
/// 3. Parent if no siblings remain
fn next_selection_after_delete(
    root: &crate::model::tree::FileNode,
    deleted_path: &[usize],
) -> Option<Vec<usize>> {
    if deleted_path.is_empty() {
        return None;
    }

    let (&deleted_idx, parent_path) = deleted_path.split_last().unwrap();

    // Navigate to the parent (after deletion)
    let parent = resolve_selected(root, parent_path)?;
    let child_count = parent.children.len();

    if child_count == 0 {
        // No children left — select the parent
        if parent_path.is_empty() {
            None // Root has no children, nothing to select
        } else {
            Some(parent_path.to_vec())
        }
    } else if deleted_idx < child_count {
        // Next sibling shifted into this index
        let mut path = parent_path.to_vec();
        path.push(deleted_idx);
        Some(path)
    } else {
        // Deleted was last — select previous sibling
        let mut path = parent_path.to_vec();
        path.push(child_count - 1);
        Some(path)
    }
}

fn resolve_selected<'a>(
    root: &'a crate::model::tree::FileNode,
    path: &[usize],
) -> Option<&'a crate::model::tree::FileNode> {
    let mut node = root;
    for &idx in path {
        node = node.children.get(idx)?;
    }
    Some(node)
}

enum DeleteAction {
    Confirm,
    Cancel,
}

fn show_delete_dialog(ctx: &egui::Context, pd: &PendingDelete) -> Option<DeleteAction> {
    let mut action = None;

    // Consume Enter/Escape before the modal so they work even without focus
    let enter_pressed = ctx.input(|i| i.key_pressed(egui::Key::Enter));
    let escape_pressed = ctx.input(|i| i.key_pressed(egui::Key::Escape));

    if enter_pressed {
        return Some(DeleteAction::Confirm);
    }
    if escape_pressed {
        return Some(DeleteAction::Cancel);
    }

    egui::Window::new("Confirm Delete")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(8.0);
                let kind = if pd.is_dir { "directory" } else { "file" };
                ui.label(format!("Delete this {}?", kind));
                ui.add_space(4.0);
                ui.strong(&pd.name);
                ui.label(format_size(pd.size));
                ui.add_space(4.0);
                ui.label(pd.fs_path.display().to_string());
                if pd.is_dir {
                    ui.add_space(4.0);
                    ui.colored_label(
                        egui::Color32::from_rgb(200, 0, 0),
                        "This will permanently delete the directory and all its contents.",
                    );
                }
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button("Delete  [Enter]").clicked() {
                        action = Some(DeleteAction::Confirm);
                    }
                    if ui.button("Cancel  [Esc]").clicked() {
                        action = Some(DeleteAction::Cancel);
                    }
                });
                ui.add_space(4.0);
            });
        });

    action
}

/// Simple folder picker using a native dialog (or fallback to hardcoded path).
fn rfd_pick_folder() -> Option<PathBuf> {
    // eframe doesn't ship a file dialog; use a simple approach
    // For now, use std::process::Command to invoke osascript
    let output = std::process::Command::new("osascript")
        .arg("-e")
        .arg("POSIX path of (choose folder with prompt \"Select folder to scan\")")
        .output()
        .ok()?;

    if output.status.success() {
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !path.is_empty() {
            return Some(PathBuf::from(path));
        }
    }
    None
}
