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
        // Global blue selection highlight
        ctx.style_mut(|style| {
            style.visuals.selection.bg_fill = egui::Color32::from_rgb(56, 132, 244);
        });

        // Scanning is synchronous — blocks the UI thread
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
                handle_delete(loaded, ctx);

                let loaded = loaded.as_mut();

                // Status bar with selection info + action buttons
                egui::TopBottomPanel::bottom("status_bar").show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(format!(
                            "{} Files",
                            format_file_count(loaded.tree.root.file_count)
                        ));
                        ui.separator();
                        ui.label(format!(
                            "{} Scanned in {:.0}ms",
                            format_size(loaded.tree.root.size),
                            loaded.scan_time_ms,
                        ));

                        // Right side: action buttons
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let has_selection = loaded.selected.is_some();

                            // Move to Trash button (red-tinted)
                            let trash_text =
                                egui::RichText::new("\u{1F5D1}").color(if has_selection {
                                    egui::Color32::from_rgb(220, 60, 60)
                                } else {
                                    egui::Color32::from_rgb(160, 120, 120)
                                });
                            let trash_btn =
                                ui.add_enabled(has_selection, egui::Button::new(trash_text));
                            if trash_btn.clicked()
                                && let Some(target) = loaded
                                    .selected
                                    .as_ref()
                                    .and_then(|sp| DeleteTarget::from_selection(&loaded.tree, sp))
                                && native_confirm_delete(
                                    target.name(),
                                    target.size,
                                    &target.fs_path,
                                    target.is_dir,
                                )
                            {
                                execute_delete(loaded, &target);
                            }

                            // Reveal in Finder button
                            let reveal_btn = ui.add_enabled(
                                has_selection,
                                egui::Button::new("\u{1F50D} Reveal in Finder"),
                            );
                            if reveal_btn.clicked()
                                && let Some(sel_path) = loaded.selected.as_ref()
                                && let Some(fs_path) = loaded.tree.build_fs_path(sel_path)
                            {
                                reveal_in_finder(&fs_path);
                            }
                        });
                    });
                });

                // Left panel: tree view
                egui::SidePanel::left("tree_view")
                    .default_width(250.0)
                    .show_separator_line(false)
                    .frame(
                        egui::Frame::side_top_panel(ctx.style().as_ref())
                            .inner_margin(egui::Margin::from(8)),
                    )
                    .show(ctx, |ui| {
                        ui::tree_view::show(ui, &loaded.tree.root, &mut loaded.selected);
                    });

                // Central panel: breadcrumb + treemap
                egui::CentralPanel::default().show(ctx, |ui| {
                    // Breadcrumb path bar (above treemap only)
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        let segments: Vec<&str> = loaded
                            .tree
                            .root_path
                            .split('/')
                            .filter(|s| !s.is_empty())
                            .collect();

                        ui.label(egui::RichText::new("\u{1F4BB}").size(13.0));
                        let last_idx = segments.len().saturating_sub(1);
                        if !segments.is_empty() {
                            ui.label(egui::RichText::new("Macintosh HD").size(13.0));
                            ui.label(
                                egui::RichText::new(" \u{203A} ")
                                    .size(13.0)
                                    .color(egui::Color32::GRAY),
                            );
                        }
                        for (i, seg) in segments.iter().enumerate() {
                            let text = if i == last_idx {
                                egui::RichText::new(*seg).size(13.0).strong()
                            } else {
                                egui::RichText::new(*seg).size(13.0)
                            };
                            ui.label(text);
                            if i < last_idx {
                                ui.label(
                                    egui::RichText::new(" \u{203A} ")
                                        .size(13.0)
                                        .color(egui::Color32::GRAY),
                                );
                            }
                        }
                    });
                    ui.add_space(2.0);

                    ui::treemap_view::show(
                        ui,
                        &mut loaded.tree,
                        &mut loaded.selected,
                        &loaded.color_map,
                        &mut loaded.cached_layout_size,
                        &mut loaded.treemap_texture,
                    );
                });
            }
        }
    }
}

/// Snapshot of a node's metadata needed for deletion.
struct DeleteTarget {
    sel_path: TreePath,
    fs_path: std::path::PathBuf,
    is_dir: bool,
    size: u64,
    file_count: u64,
    dir_count: u64,
}

impl DeleteTarget {
    /// Resolve the selected node into a DeleteTarget, or None if the path is invalid.
    fn from_selection(tree: &FileTree, sel_path: &[usize]) -> Option<Self> {
        let fs_path = tree.build_fs_path(sel_path)?;
        let node = resolve_selected(&tree.root, sel_path)?;
        Some(Self {
            sel_path: sel_path.to_vec(),
            fs_path,
            is_dir: node.is_dir,
            size: node.size,
            file_count: node.file_count,
            dir_count: node.dir_count,
        })
    }

    fn name(&self) -> &str {
        self.fs_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
    }
}

/// Handle Delete/Backspace when something is selected.
/// ⌘Delete: delete immediately (no confirmation).
/// Delete alone: show native confirmation dialog.
fn handle_delete(loaded: &mut LoadedState, ctx: &egui::Context) {
    let Some(sel_path) = loaded.selected.as_ref() else {
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
    let Some(target) = DeleteTarget::from_selection(&loaded.tree, sel_path) else {
        return;
    };
    if !cmd_delete
        && !native_confirm_delete(target.name(), target.size, &target.fs_path, target.is_dir)
    {
        return;
    }
    execute_delete(loaded, &target);
}

fn execute_delete(loaded: &mut LoadedState, target: &DeleteTarget) {
    let result = if target.is_dir {
        std::fs::remove_dir_all(&target.fs_path)
    } else {
        std::fs::remove_file(&target.fs_path)
    };
    match result {
        Ok(()) => {
            loaded.tree.subtract_from_ancestors(
                &target.sel_path,
                target.size,
                target.file_count,
                target.dir_count,
            );
            loaded.tree.remove_at_path(&target.sel_path);
            loaded.tree.rebuild_extensions();
            loaded.color_map = ColorMap::from_extensions(&loaded.tree.extensions);
            loaded.selected = next_selection_after_delete(&loaded.tree.root, &target.sel_path);
            loaded.cached_layout_size = None;
            loaded.treemap_texture = None;
        }
        Err(e) => {
            eprintln!("Failed to delete {:?}: {}", target.fs_path, e);
        }
    }
}

/// Render the three-pane layout with empty panels (same IDs as Loaded state).
fn show_empty_panes(ctx: &egui::Context) {
    egui::TopBottomPanel::bottom("status_bar").show(ctx, |_ui| {});

    egui::SidePanel::left("tree_view")
        .default_width(250.0)
        .show_separator_line(false)
        .frame(
            egui::Frame::side_top_panel(ctx.style().as_ref()).inner_margin(egui::Margin::from(8)),
        )
        .show(ctx, |ui| {
            ui::tree_view::show_branding(ui);
        });

    egui::CentralPanel::default().show(ctx, |_ui| {});
}

/// After deleting the node at `deleted_path`, determine what to select next.
fn next_selection_after_delete(
    root: &crate::model::tree::FileNode,
    deleted_path: &[usize],
) -> Option<TreePath> {
    let (&deleted_idx, parent_path) = deleted_path.split_last()?;

    let parent = resolve_selected(root, parent_path)?;
    let child_count = parent.children.len();

    if child_count == 0 {
        if parent_path.is_empty() {
            None
        } else {
            Some(parent_path.to_vec())
        }
    } else if deleted_idx < child_count {
        let mut path = parent_path.to_vec();
        path.push(deleted_idx);
        Some(path)
    } else {
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

fn reveal_in_finder(path: &std::path::Path) {
    let _ = std::process::Command::new("open")
        .arg("-R")
        .arg(path)
        .spawn();
}

fn format_file_count(count: u64) -> String {
    if count >= 1_000_000 {
        format!("{:.1}M", count as f64 / 1_000_000.0)
    } else if count >= 1_000 {
        // Format with comma separators
        let s = count.to_string();
        let mut result = String::new();
        for (i, c) in s.chars().rev().enumerate() {
            if i > 0 && i % 3 == 0 {
                result.push(',');
            }
            result.push(c);
        }
        result.chars().rev().collect()
    } else {
        count.to_string()
    }
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

/// Folder picker starting at $HOME — used on startup.
fn pick_folder_at_home() -> Option<PathBuf> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/Users".to_string());
    rfd::FileDialog::new()
        .set_title("Select folder to scan")
        .set_directory(&home)
        .pick_folder()
}
