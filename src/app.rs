use std::path::PathBuf;
use std::time::Instant;

use eframe::egui;

use crate::format_size;
use crate::model::color::ColorMap;
use crate::model::tree::{FileTree, TreePath};
use crate::ui;

pub struct App {
    state: AppState,
}

enum AppState {
    WaitingForPicker { frames: u8 },
    Scanning { path: PathBuf, start_time: Instant },
    Loaded(Box<LoadedState>),
}

struct LoadedState {
    tree: FileTree,
    color_map: ColorMap,
    selected: Option<TreePath>,
    scan_time_ms: f64,
    cached_layout_size: Option<(f32, f32)>,
    treemap_texture: Option<egui::TextureHandle>,
}

impl App {
    pub fn new(_cc: &eframe::CreationContext<'_>, initial_path: Option<String>) -> Self {
        let mut app = Self {
            state: AppState::WaitingForPicker { frames: 2 },
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
        if let AppState::Scanning {
            ref path,
            start_time,
        } = self.state
        {
            let tree = FileTree::scan(path);
            let scan_time_ms = start_time.elapsed().as_secs_f64() * 1000.0;
            let color_map = ColorMap::from_extensions(&tree.extensions);
            self.state = AppState::Loaded(Box::new(LoadedState {
                tree,
                color_map,
                selected: None,
                scan_time_ms,
                cached_layout_size: None,
                treemap_texture: None,
            }));
        }

        egui::TopBottomPanel::top("menu_bar").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("Open Folder...").clicked() {
                        if let Some(path) = pick_folder() {
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
            AppState::WaitingForPicker { frames } => {
                show_empty_panes(ctx);

                if *frames > 0 {
                    *frames -= 1;
                    ctx.request_repaint();
                } else if *frames == 0 {
                    // Prevent re-entry after the blocking dialog returns
                    *frames = u8::MAX;
                    let result = pick_folder_at_home();
                    if let Some(path) = result {
                        self.start_scan(path);
                    } else {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
                // frames == u8::MAX: dialog was dismissed, waiting for close
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
                } = loaded.as_mut();

                handle_delete(
                    ctx,
                    tree,
                    color_map,
                    selected,
                    cached_layout_size,
                    treemap_texture,
                );

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

/// Handle Delete/Backspace when something is selected.
/// ⌘Delete: delete immediately (no confirmation).
/// Delete alone: show native confirmation dialog.
fn handle_delete(
    ctx: &egui::Context,
    tree: &mut FileTree,
    color_map: &mut ColorMap,
    selected: &mut Option<TreePath>,
    cached_layout_size: &mut Option<(f32, f32)>,
    treemap_texture: &mut Option<egui::TextureHandle>,
) {
    let Some(sel_path) = selected.as_ref() else {
        return;
    };
    let (cmd_delete, bare_delete) = ctx.input(|i| {
        let del = i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace);
        let cmd = i.modifiers.command;
        (del && cmd, del && !cmd)
    });
    if !(cmd_delete || bare_delete) {
        return;
    }
    let (Some(fs_path), Some(node)) = (
        tree.build_fs_path(sel_path),
        resolve_selected(&tree.root, sel_path),
    ) else {
        return;
    };

    let name = node.name.clone();
    let size = node.size;
    let is_dir = node.is_dir;
    let file_count = node.file_count;
    let dir_count = node.dir_count;
    let sel_path = sel_path.clone();

    if !cmd_delete && !native_confirm_delete(&name, size, &fs_path, is_dir) {
        return;
    }

    let result = if is_dir {
        std::fs::remove_dir_all(&fs_path)
    } else {
        std::fs::remove_file(&fs_path)
    };
    match result {
        Ok(()) => {
            tree.subtract_from_ancestors(&sel_path, size, file_count, dir_count);
            tree.remove_at_path(&sel_path);
            tree.rebuild_extensions();
            *color_map = ColorMap::from_extensions(&tree.extensions);
            *selected = next_selection_after_delete(&tree.root, &sel_path);
            *cached_layout_size = None;
            *treemap_texture = None;
        }
        Err(e) => {
            eprintln!("Failed to delete {:?}: {}", fs_path, e);
        }
    }
}

/// Render the three-pane layout with empty panels (same IDs as Loaded state).
fn show_empty_panes(ctx: &egui::Context) {
    egui::TopBottomPanel::bottom("status_bar").show(ctx, |_ui| {});

    egui::SidePanel::right("extensions")
        .default_width(220.0)
        .show(ctx, |ui| {
            ui.heading("File Types");
            ui.separator();
        });

    egui::SidePanel::left("tree_view")
        .default_width(350.0)
        .show(ctx, |ui| {
            ui.heading("Directory Tree");
            ui.separator();
        });

    egui::CentralPanel::default().show(ctx, |_ui| {});
}

/// After deleting the node at `deleted_path`, determine what to select next:
/// 1. Same index (next sibling shifted into place) if it exists
/// 2. Previous sibling if deleted was last child
/// 3. Parent if no siblings remain
fn next_selection_after_delete(
    root: &crate::model::tree::FileNode,
    deleted_path: &[usize],
) -> Option<TreePath> {
    let (&deleted_idx, parent_path) = deleted_path.split_last()?;

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

/// Show a native macOS alert for delete confirmation. Returns true if user clicked "Delete".
fn native_confirm_delete(name: &str, size: u64, fs_path: &std::path::Path, is_dir: bool) -> bool {
    let kind = if is_dir { "directory" } else { "file" };
    let escaped_name = applescript_escape(name);
    let escaped_path = applescript_escape(&fs_path.display().to_string());
    let size_str = format_size(size);

    let mut message = format!("{} ({})\n{}", escaped_name, size_str, escaped_path);
    if is_dir {
        message.push_str("\n\nThis will permanently delete the directory and all its contents.");
    }

    let script = format!(
        r#"display alert "Delete this {}?" message "{}" as critical buttons {{"Cancel", "Delete"}} default button "Cancel""#,
        kind, message,
    );

    let output = std::process::Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .output();

    match output {
        Ok(out) if out.status.success() => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            stdout.contains("button returned:Delete")
        }
        _ => false,
    }
}

/// Escape a string for use inside AppleScript double-quoted strings.
fn applescript_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Folder picker using native NSOpenPanel.
fn pick_folder() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Select folder to scan")
        .pick_folder()
}

/// Folder picker starting at $HOME — used on startup.
fn pick_folder_at_home() -> Option<PathBuf> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/Users".to_string());
    rfd::FileDialog::new()
        .set_title("Select folder to scan")
        .set_directory(&home)
        .pick_folder()
}
