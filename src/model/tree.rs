use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use crate::scan::getattrlistbulk::{self, DirEntry};

/// Index path from root to a node in the tree (e.g. [2, 0, 1] = root's 3rd child → 1st child → 2nd child).
pub type TreePath = Vec<usize>;

thread_local! {
    static LOCAL_EXT_MAP: RefCell<HashMap<Box<str>, u64>> = RefCell::new(HashMap::new());
}

fn raw_extension(name: &str) -> &str {
    match name.rsplit_once('.') {
        Some((_, ext)) if !ext.is_empty() => ext,
        _ => "",
    }
}

/// A node in the file tree. Uses compact representation (Box<str> + Box<[T]>)
/// as validated by memory benchmarks: 40 bytes/struct, ~78 bytes RSS/node.
pub struct FileNode {
    pub name: Box<str>,
    pub size: u64,
    pub is_dir: bool,
    pub children: Box<[FileNode]>,
    /// Treemap rectangle, set during layout.
    pub rect: treemap::Rect,
    /// Cached file count (1 for files, sum of children for dirs).
    pub file_count: u64,
    /// Cached directory count (0 for files, 1 + sum of children for dirs).
    pub dir_count: u64,
}

impl FileNode {
    /// Get the file extension, or empty string for dirs/extensionless files.
    pub fn extension(&self) -> &str {
        if self.is_dir {
            ""
        } else {
            raw_extension(&self.name)
        }
    }

    /// Remove the child at `index` from this node's children, updating size and counts.
    /// Returns the removed child.
    pub fn remove_child(&mut self, index: usize) -> FileNode {
        let mut children = std::mem::take(&mut self.children).into_vec();
        let removed = children.remove(index);
        self.size = self.size.saturating_sub(removed.size);
        self.file_count = self.file_count.saturating_sub(removed.file_count);
        self.dir_count = self.dir_count.saturating_sub(removed.dir_count);
        self.children = children.into();
        removed
    }
}

/// The complete scanned file tree with precomputed extension statistics.
pub struct FileTree {
    pub root: FileNode,
    pub root_path: String,
    /// Extension -> total bytes mapping, sorted by size descending.
    pub extensions: Vec<(Box<str>, u64)>,
}

impl FileTree {
    /// Build a file tree by scanning the given path using getattrlistbulk.
    pub fn scan(root: &Path) -> Self {
        let ext_map = Mutex::new(HashMap::<Box<str>, u64>::new());
        let root_node = build_root_node(root);

        // Drain the main thread's local ext map
        LOCAL_EXT_MAP.with(|m| {
            let local = m.replace(HashMap::new());
            if !local.is_empty() {
                let mut global = ext_map.lock().unwrap_or_else(|e| e.into_inner());
                for (k, v) in local {
                    *global.entry(k).or_default() += v;
                }
            }
        });

        // Drain all rayon worker thread local ext maps
        rayon::broadcast(|_| {
            LOCAL_EXT_MAP.with(|m| {
                let local = m.replace(HashMap::new());
                if !local.is_empty() {
                    let mut global = ext_map.lock().unwrap_or_else(|e| e.into_inner());
                    for (k, v) in local {
                        *global.entry(k).or_default() += v;
                    }
                }
            });
        });

        let mut extensions: Vec<(Box<str>, u64)> = ext_map
            .into_inner()
            .unwrap_or_else(|e| e.into_inner())
            .into_iter()
            .collect();
        extensions.sort_unstable_by(|a, b| b.1.cmp(&a.1));

        FileTree {
            root: root_node,
            root_path: root.display().to_string(),
            extensions,
        }
    }

    /// Build the full filesystem path for a node identified by index path.
    pub fn build_fs_path(&self, path: &[usize]) -> Option<std::path::PathBuf> {
        let mut fs_path = std::path::PathBuf::from(&self.root_path);
        let mut node = &self.root;
        for &idx in path {
            let child = node.children.get(idx)?;
            fs_path.push(&*child.name);
            node = child;
        }
        Some(fs_path)
    }

    /// Remove the node at the given index path from the tree, updating all ancestor sizes/counts.
    /// Returns the removed node, or None if the path is invalid.
    pub fn remove_at_path(&mut self, path: &[usize]) -> Option<FileNode> {
        let (&child_idx, parent_path) = path.split_last()?;

        // Navigate to the parent
        let mut node = &mut self.root;
        for &idx in parent_path {
            node = node.children.get_mut(idx)?;
        }

        if child_idx >= node.children.len() {
            return None;
        }

        Some(node.remove_child(child_idx))
    }

    /// Propagate a size/count reduction up the ancestor chain (excluding the node itself).
    pub fn subtract_from_ancestors(
        &mut self,
        path: &[usize],
        size: u64,
        file_count: u64,
        dir_count: u64,
    ) {
        // The direct parent is already updated by remove_child; update grandparents and above.
        let mut node = &mut self.root;
        // Update root
        node.size = node.size.saturating_sub(size);
        node.file_count = node.file_count.saturating_sub(file_count);
        node.dir_count = node.dir_count.saturating_sub(dir_count);
        // Update intermediate ancestors (not the direct parent, which remove_child handles)
        if path.len() >= 2 {
            for &idx in &path[..path.len() - 2] {
                if let Some(child) = node.children.get_mut(idx) {
                    child.size = child.size.saturating_sub(size);
                    child.file_count = child.file_count.saturating_sub(file_count);
                    child.dir_count = child.dir_count.saturating_sub(dir_count);
                    node = child;
                } else {
                    break;
                }
            }
        }
    }

    /// Rebuild extension statistics from the current tree.
    pub fn rebuild_extensions(&mut self) {
        let mut ext_map: HashMap<Box<str>, u64> = HashMap::new();
        collect_extensions(&self.root, &mut ext_map);
        let mut extensions: Vec<(Box<str>, u64)> = ext_map.into_iter().collect();
        extensions.sort_unstable_by(|a, b| b.1.cmp(&a.1));
        self.extensions = extensions;
    }
}

fn collect_extensions(node: &FileNode, map: &mut HashMap<Box<str>, u64>) {
    if !node.is_dir {
        let ext = node.extension();
        if !ext.is_empty() {
            *map.entry(ext.into()).or_default() += node.size;
        } else {
            *map.entry("(no ext)".into()).or_default() += node.size;
        }
    }
    for child in node.children.iter() {
        collect_extensions(child, map);
    }
}

fn build_root_node(path: &Path) -> FileNode {
    let fd = getattrlistbulk::open_dir(path);
    if fd < 0 {
        eprintln!(
            "Warning: could not open directory {:?} (permission denied or not found)",
            path
        );
    }
    let name: Box<str> = path.display().to_string().into();
    let node = build_node_fd(fd, name);
    getattrlistbulk::close_dir(fd);
    node
}

/// Build a FileNode from an already-opened directory fd.
/// `node_name` is the display name for this node.
fn build_node_fd(parent_fd: libc::c_int, node_name: Box<str>) -> FileNode {
    use rayon::prelude::*;

    let entries = getattrlistbulk::scan_dir_entries_fd(parent_fd);

    // Separate files and directories
    let mut file_nodes: Vec<FileNode> = Vec::new();
    let mut dir_names: Vec<&DirEntry> = Vec::new();
    let mut total_size: u64 = 0;
    let mut total_file_count: u64 = 0;

    for entry in &entries {
        if entry.is_dir {
            dir_names.push(entry);
        } else {
            total_size += entry.file_size;
            total_file_count += 1;
            LOCAL_EXT_MAP.with(|m| {
                let mut map = m.borrow_mut();
                let ext = raw_extension(&entry.name);
                let key: Box<str> = if ext.is_empty() {
                    "(no ext)".into()
                } else {
                    ext.into()
                };
                *map.entry(key).or_default() += entry.file_size;
            });
            file_nodes.push(FileNode {
                name: entry.name.clone(),
                size: entry.file_size,
                is_dir: false,
                children: Box::new([]),
                rect: treemap::Rect::new(),
                file_count: 1,
                dir_count: 0,
            });
        }
    }

    // Recurse into subdirectories — use openat() relative to parent fd
    let build_child = |entry: &&DirEntry| -> FileNode {
        let child_fd = getattrlistbulk::openat_dir(parent_fd, &entry.name);
        let node = build_node_fd(child_fd, entry.name.clone());
        getattrlistbulk::close_dir(child_fd);
        node
    };

    let dir_nodes: Vec<FileNode> = if dir_names.len() >= 2 {
        dir_names.par_iter().map(build_child).collect()
    } else {
        dir_names.iter().map(build_child).collect()
    };

    let mut total_dir_count: u64 = 0;
    for child in &dir_nodes {
        total_size += child.size;
        total_file_count += child.file_count;
        total_dir_count += child.dir_count;
    }

    let mut children: Vec<FileNode> = Vec::with_capacity(file_nodes.len() + dir_nodes.len());
    children.extend(file_nodes);
    children.extend(dir_nodes);

    children.sort_unstable_by(|a, b| b.size.cmp(&a.size));

    FileNode {
        name: node_name,
        size: total_size,
        is_dir: true,
        children: children.into(),
        rect: treemap::Rect::new(),
        file_count: total_file_count,
        dir_count: total_dir_count + 1,
    }
}
