use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::scan::getattrlistbulk::{self, DirEntry};

/// Index path from root to a node in the tree (e.g. [2, 0, 1] = root's 3rd child → 1st child → 2nd child).
pub type TreePath = Vec<usize>;

/// Fixed-point total for [`ScanProgress::work_done`]; reaching it means 100%.
pub const WORK_TOTAL: u64 = 1 << 62;

/// Live counters for a running scan. Shared between the scanning threads and the UI.
#[derive(Default)]
pub struct ScanProgress {
    pub files: AtomicU64,
    pub dirs: AtomicU64,
    pub bytes: AtomicU64,
    /// Heuristic completion in units of [`WORK_TOTAL`]. Each directory owns a
    /// share of the total which is split among its subdirectories (weighted by
    /// their entry counts); leaf directories add their share when finished.
    pub work_done: AtomicU64,
    pub cancel: AtomicBool,
}

impl ScanProgress {
    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// Heuristic fraction complete in 0.0..=1.0.
    pub fn work_fraction(&self) -> f64 {
        self.work_done.load(Ordering::Relaxed) as f64 / WORK_TOTAL as f64
    }
}

/// Per-scan state threaded through the recursive tree build.
struct ScanCtx<'a> {
    progress: &'a ScanProgress,
    /// Absolute paths that must not be descended into (e.g. firmlink duplicates).
    excluded: Vec<PathBuf>,
}

impl ScanCtx<'_> {
    /// Returns the child's absolute path if it still needs tracking (i.e. it is a
    /// strict ancestor of an excluded path), and whether the child is excluded.
    fn classify(&self, parent: Option<&Path>, name: &str) -> (Option<PathBuf>, bool) {
        let Some(parent) = parent else {
            return (None, false);
        };
        let child = parent.join(name);
        if self.excluded.contains(&child) {
            return (None, true);
        }
        let track = self.excluded.iter().any(|e| e.starts_with(&child));
        (track.then_some(child), false)
    }
}

/// Paths below `root` that are not descended into:
/// - mount points that are hidden from Finder (`nobrowse`) or not local. These
///   are system volumes (`/System/Volumes/*`, whose data is already visible
///   through firmlinks, so scanning it would double time and size), simulator
///   runtimes, `/dev`, and virtual FSKit file systems such as the connected-
///   device view in `~/Library/Developer/CoreDevice/DeviceFS`, which is very
///   slow to traverse and doesn't use local disk space;
/// - `/Volumes` when scanning `/`, since it holds other disks.
fn default_exclusions(root: &Path) -> Vec<PathBuf> {
    let mut excluded: Vec<PathBuf> = getattrlistbulk::mount_points()
        .into_iter()
        .filter(|m| (m.dont_browse || !m.local) && m.path != root && m.path.starts_with(root))
        .map(|m| m.path)
        .collect();
    if root == Path::new("/") {
        excluded.push(PathBuf::from("/Volumes"));
    }
    excluded
}

pub(crate) fn raw_extension(name: &str) -> &str {
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

    /// Walk a path of child indices to reach a descendant node.
    pub fn resolve_path(&self, path: &[usize]) -> Option<&FileNode> {
        let mut node = self;
        for &idx in path {
            node = node.children.get(idx)?;
        }
        Some(node)
    }

    /// Mutable variant of [`resolve_path`](Self::resolve_path).
    pub fn resolve_path_mut(&mut self, path: &[usize]) -> Option<&mut FileNode> {
        let mut node = self;
        for &idx in path {
            node = node.children.get_mut(idx)?;
        }
        Some(node)
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
        Self::scan_with_progress(root, &ScanProgress::default())
    }

    /// Scan `root`, reporting progress through `progress`. If `progress.cancel`
    /// is set mid-scan the returned tree is incomplete and should be discarded.
    pub fn scan_with_progress(root: &Path, progress: &ScanProgress) -> Self {
        let ctx = ScanCtx {
            progress,
            excluded: default_exclusions(root),
        };
        let root_node = build_root_node(root, &ctx);
        Self::from_root(root_node, root.display().to_string())
    }

    /// Wrap a root node into a tree, computing extension statistics.
    pub fn from_root(root: FileNode, root_path: String) -> Self {
        let mut tree = FileTree {
            root,
            root_path,
            extensions: Vec::new(),
        };
        tree.rebuild_extensions();
        tree
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

    /// Remove the node at the given index path, updating sizes/counts of all
    /// ancestors. Returns the removed node, or None if the path is invalid.
    pub fn remove_at_path(&mut self, path: &[usize]) -> Option<FileNode> {
        let (&child_idx, parent_path) = path.split_last()?;
        let removed = {
            let parent = self.root.resolve_path_mut(parent_path)?;
            if child_idx >= parent.children.len() {
                return None;
            }
            parent.remove_child(child_idx)
        };
        // remove_child updated the direct parent; now update the rest.
        let mut node = &mut self.root;
        for &idx in parent_path {
            node.size = node.size.saturating_sub(removed.size);
            node.file_count = node.file_count.saturating_sub(removed.file_count);
            node.dir_count = node.dir_count.saturating_sub(removed.dir_count);
            node = &mut node.children[idx];
        }
        Some(removed)
    }

    /// Names of the nodes along an index path (excluding the root).
    pub fn names_for_path(&self, path: &[usize]) -> Option<Vec<Box<str>>> {
        let mut names = Vec::with_capacity(path.len());
        let mut node = &self.root;
        for &idx in path {
            node = node.children.get(idx)?;
            names.push(node.name.clone());
        }
        Some(names)
    }

    /// Resolve a sequence of child names (excluding the root) to an index path.
    pub fn path_for_names<S: AsRef<str>>(&self, names: &[S]) -> Option<TreePath> {
        let mut path = Vec::with_capacity(names.len());
        let mut node = &self.root;
        for name in names {
            let idx = node
                .children
                .iter()
                .position(|c| &*c.name == name.as_ref())?;
            path.push(idx);
            node = &node.children[idx];
        }
        Some(path)
    }

    /// Resolve an absolute filesystem path inside this tree to an index path.
    pub fn path_for_fs_path(&self, fs_path: &Path) -> Option<TreePath> {
        let rel = fs_path.strip_prefix(&self.root_path).ok()?;
        let names: Vec<String> = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        self.path_for_names(&names)
    }

    /// Rebuild extension statistics from the current tree.
    pub fn rebuild_extensions(&mut self) {
        use rayon::prelude::*;

        let mut ext_map: HashMap<Box<str>, u64> = self
            .root
            .children
            .par_iter()
            .fold(HashMap::new, |mut map, child| {
                collect_extensions(child, &mut map);
                map
            })
            .reduce(HashMap::new, |mut a, b| {
                for (k, v) in b {
                    *a.entry(k).or_default() += v;
                }
                a
            });
        if !self.root.is_dir {
            collect_extensions(&self.root, &mut ext_map);
        }
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

fn build_root_node(path: &Path, ctx: &ScanCtx) -> FileNode {
    let fd = getattrlistbulk::open_dir(path);
    if fd < 0 {
        eprintln!(
            "Warning: could not open directory {:?} (permission denied or not found)",
            path
        );
    }
    let name: Box<str> = path.display().to_string().into();
    let tracked = (!ctx.excluded.is_empty()).then(|| path.to_path_buf());
    let node = build_node_fd(fd, name, ctx, WORK_TOTAL, tracked.as_deref());
    getattrlistbulk::close_dir(fd);
    node
}

fn empty_dir(name: Box<str>) -> FileNode {
    FileNode {
        name,
        size: 0,
        is_dir: true,
        children: Box::new([]),
        rect: treemap::Rect::new(),
        file_count: 0,
        dir_count: 1,
    }
}

/// Build a FileNode from an already-opened directory fd.
/// `node_name` is the display name for this node, `work` is this directory's
/// share of [`WORK_TOTAL`] for progress reporting, and `abs_path` is set only
/// while the directory is an ancestor of an excluded path.
fn build_node_fd(
    parent_fd: libc::c_int,
    node_name: Box<str>,
    ctx: &ScanCtx,
    work: u64,
    abs_path: Option<&Path>,
) -> FileNode {
    use rayon::prelude::*;

    let progress = ctx.progress;
    if progress.is_cancelled() {
        return empty_dir(node_name);
    }

    let entries = getattrlistbulk::scan_dir_entries_fd(parent_fd);

    // Separate files and directories
    let mut file_nodes: Vec<FileNode> = Vec::new();
    let mut dir_entries: Vec<(DirEntry, Option<PathBuf>)> = Vec::new();
    let mut total_size: u64 = 0;

    for entry in entries {
        if entry.is_dir {
            let (tracked, excluded) = ctx.classify(abs_path, &entry.name);
            if !excluded {
                dir_entries.push((entry, tracked));
            }
        } else {
            total_size += entry.file_size;
            file_nodes.push(FileNode {
                name: entry.name,
                size: entry.file_size,
                is_dir: false,
                children: Box::new([]),
                rect: treemap::Rect::new(),
                file_count: 1,
                dir_count: 0,
            });
        }
    }
    let total_file_count = file_nodes.len() as u64;

    progress
        .files
        .fetch_add(total_file_count, Ordering::Relaxed);
    progress.dirs.fetch_add(1, Ordering::Relaxed);
    progress.bytes.fetch_add(total_size, Ordering::Relaxed);

    // Split this directory's work share among subdirectories, weighted by
    // their entry counts. Leaves report their share as done.
    let shares: Vec<u64> = if dir_entries.is_empty() {
        progress.work_done.fetch_add(work, Ordering::Relaxed);
        Vec::new()
    } else {
        let weights: Vec<u128> = dir_entries
            .iter()
            .map(|(e, _)| 1 + e.entry_count as u128)
            .collect();
        let sum: u128 = weights.iter().sum();
        let mut shares: Vec<u64> = weights
            .iter()
            .map(|&w| (work as u128 * w / sum) as u64)
            .collect();
        // Give rounding remainder to the first child so shares sum exactly.
        let assigned: u64 = shares.iter().sum();
        shares[0] += work - assigned;
        shares
    };

    // Recurse into subdirectories — use openat() relative to parent fd
    let build_child = |((entry, tracked), share): (&(DirEntry, Option<PathBuf>), &u64)| {
        let child_fd = getattrlistbulk::openat_dir(parent_fd, &entry.name);
        let node = build_node_fd(
            child_fd,
            entry.name.clone(),
            ctx,
            *share,
            tracked.as_deref(),
        );
        getattrlistbulk::close_dir(child_fd);
        node
    };

    let dir_nodes: Vec<FileNode> = if dir_entries.len() >= 2 {
        dir_entries
            .par_iter()
            .zip(shares.par_iter())
            .map(build_child)
            .collect()
    } else {
        dir_entries
            .iter()
            .zip(shares.iter())
            .map(build_child)
            .collect()
    };

    let mut total_file_count = total_file_count;
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
