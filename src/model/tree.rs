use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
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

/// Bucket key for extension stats: the raw extension, or the sentinel for
/// extensionless files. Single owner of the `"(no ext)"` sentinel.
fn ext_bucket(name: &str) -> Box<str> {
    let ext = raw_extension(name);
    if ext.is_empty() {
        "(no ext)".into()
    } else {
        ext.into()
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

        // Merge one thread's local ext map into the global one.
        let merge = |local: HashMap<Box<str>, u64>| {
            if !local.is_empty() {
                let mut global = ext_map.lock().unwrap_or_else(|e| e.into_inner());
                for (k, v) in local {
                    *global.entry(k).or_default() += v;
                }
            }
        };

        // Drain the main thread's local ext map, then all rayon workers'.
        LOCAL_EXT_MAP.with(|m| merge(m.replace(HashMap::new())));
        rayon::broadcast(|_| LOCAL_EXT_MAP.with(|m| merge(m.replace(HashMap::new()))));

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
        // (extension() returns "" for dirs, but this branch only runs for files,
        // so it agrees with raw_extension here.)
        *map.entry(ext_bucket(&node.name)).or_default() += node.size;
    }
    for child in node.children.iter() {
        collect_extensions(child, map);
    }
}

/// Identity of a visited directory: (device, inode).
/// Firmlinks expose the same directory at two paths (e.g. `/Users` and
/// `/System/Volumes/Data/Users`) with identical dev+ino; without tracking,
/// a scan covering both views counts everything twice.
type VisitedDirs = Mutex<HashSet<(u64, u64)>>;

/// Per-scan dedup state shared across traversal threads.
struct ScanCtx {
    visited: VisitedDirs,
    /// Firmlink table: Data-relative target -> lowercased canonical absolute
    /// path (e.g. `Users` -> `/users`). Empty when the table is unreadable,
    /// in which case only `visited` applies.
    firmlinks: HashMap<Box<str>, Box<str>>,
    /// (dev, ino) of `/System/Volumes/Data`, or None when `firmlinks` is empty.
    data_root: Option<(u64, u64)>,
    /// Lowercased canonicalized scan root, for containment checks.
    scan_root: Box<str>,
}

/// (dev, ino) identity of an open directory fd, or None if it can't be stated.
fn dir_ident(fd: libc::c_int) -> Option<(u64, u64)> {
    if fd < 0 {
        return None;
    }
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd, &mut st) } != 0 {
        return None;
    }
    Some((st.st_dev as u64, st.st_ino as u64))
}

/// Claim an identity for traversal. Returns false when already visited
/// through another path. Fails open (returns true) when unstated.
fn claim_ident(ident: Option<(u64, u64)>, visited: &VisitedDirs) -> bool {
    match ident {
        None => true,
        Some(key) => visited
            .lock()
            .map(|mut guard| guard.insert(key))
            .unwrap_or_else(|e| e.into_inner().insert(key)),
    }
}

/// Parse firmlink pairs (Data-relative target -> lowercased canonical absolute
/// path) from the text of the system table. Skips blanks, comments, and
/// malformed lines.
fn parse_firmlinks(text: &str) -> HashMap<Box<str>, Box<str>> {
    let mut map = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((canonical, target)) = line.split_once('\t') else {
            continue;
        };
        let target = target.trim().trim_matches('/');
        let canonical = canonical.trim().trim_matches('/');
        if target.is_empty() || canonical.is_empty() {
            continue;
        }
        map.insert(
            target.into(),
            format!("/{}", canonical.to_lowercase()).into(),
        );
    }
    map
}

/// Load firmlink pairs from the system table. Empty map when unreadable.
fn load_firmlinks() -> HashMap<Box<str>, Box<str>> {
    match std::fs::read_to_string("/usr/share/firmlinks") {
        Ok(text) => parse_firmlinks(&text),
        Err(_) => HashMap::new(),
    }
}

/// Whether the Data-relative path `rel` (e.g. `Users/gregoreesmaa`) is a mirror
/// whose canonical path is also being scanned (i.e. under the scan root).
/// Then the canonical view covers it and it can be skipped — deterministically.
/// When the canonical is outside the scan, the mirror is kept: it is the only
/// view in this scan, so skipping it would lose content. Any matching target
/// counts, from `rel` itself up through its parents.
fn is_mirror_in_scan(rel: &str, ctx: &ScanCtx) -> bool {
    let root: &str = &ctx.scan_root;
    let mut prefix = rel;
    loop {
        if let Some(canon) = ctx.firmlinks.get(prefix) {
            let canon: &str = canon;
            // Byte comparison is exact because both sides arrive canonicalized
            // and lowercased; the length check keeps "/users2" off "/users".
            if root == "/"
                || canon == root
                || (canon.len() > root.len()
                    && canon.starts_with(root)
                    && canon.as_bytes()[root.len()] == b'/')
            {
                return true;
            }
        }
        match prefix.rsplit_once('/') {
            Some((parent, _)) => prefix = parent,
            None => return false,
        }
    }
}

/// Data-relative path of child `name` given the parent's: Some("") marks the
/// Data volume root itself, None means outside the Data volume (the common
/// case — no allocation happens there either).
fn child_data_rel(
    name: &str,
    ident: Option<(u64, u64)>,
    parent_rel: Option<&str>,
    ctx: &ScanCtx,
) -> Option<String> {
    match (ident, parent_rel) {
        (Some(id), _) if ctx.data_root == Some(id) => Some(String::new()),
        (_, Some(parent)) => Some(if parent.is_empty() {
            name.to_string()
        } else {
            format!("{parent}/{name}")
        }),
        _ => None,
    }
}

/// Build the dedup context for a scan rooted at `path`.
fn make_scan_ctx(path: &Path) -> ScanCtx {
    // Canonicalize once: resolves relative roots and symlinks (a symlinked
    // root may really live inside the Data volume) for the containment check.
    let scan_root = std::fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .to_lowercase()
        .into();
    let firmlinks = load_firmlinks();
    let data_root = if firmlinks.is_empty() {
        None
    } else {
        let data_fd = getattrlistbulk::open_dir(Path::new("/System/Volumes/Data"));
        let ident = dir_ident(data_fd);
        getattrlistbulk::close_dir(data_fd);
        ident
    };
    ScanCtx {
        visited: VisitedDirs::default(),
        firmlinks,
        data_root,
        scan_root,
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
    let ctx = make_scan_ctx(path);
    let root_ident = dir_ident(fd);
    claim_ident(root_ident, &ctx.visited);
    // The root behaves like a child of nowhere: its rel is set only when
    // the root IS the Data volume.
    let root_rel = child_data_rel("", root_ident, None, &ctx);
    let name: Box<str> = path.display().to_string().into();
    let node = build_node_fd(fd, name, &ctx, root_rel.as_deref());
    getattrlistbulk::close_dir(fd);
    node
}

/// Build a FileNode from an already-opened directory fd.
/// `node_name` is the display name for this node.
/// `ctx` dedupes directories reachable via multiple paths (firmlinks);
/// `data_rel` is this directory's path relative to `/System/Volumes/Data`
/// (None when outside the Data volume — the common case, zero cost).
fn build_node_fd(
    parent_fd: libc::c_int,
    node_name: Box<str>,
    ctx: &ScanCtx,
    data_rel: Option<&str>,
) -> FileNode {
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
                *map.entry(ext_bucket(&entry.name)).or_default() += entry.file_size;
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

    // Recurse into subdirectories — use openat() relative to parent fd.
    // Two dedup layers: firmlink mirrors under /System/Volumes/Data whose
    // canonical is also in this scan are skipped (deterministic), and
    // anything already visited via another path is skipped as well
    // (bind mounts and friends — first claim wins).
    let build_child = |entry: &&DirEntry| -> Option<FileNode> {
        let child_fd = getattrlistbulk::openat_dir(parent_fd, &entry.name);
        let ident = dir_ident(child_fd);
        let child_rel = child_data_rel(&entry.name, ident, data_rel, ctx);
        if let Some(ref rel) = child_rel {
            if !rel.is_empty() && is_mirror_in_scan(rel, ctx) {
                getattrlistbulk::close_dir(child_fd);
                return None;
            }
        }
        if !claim_ident(ident, &ctx.visited) {
            getattrlistbulk::close_dir(child_fd);
            return None;
        }
        let node = build_node_fd(child_fd, entry.name.clone(), ctx, child_rel.as_deref());
        getattrlistbulk::close_dir(child_fd);
        Some(node)
    };

    let dir_nodes: Vec<FileNode> = if dir_names.len() >= 2 {
        dir_names.par_iter().filter_map(build_child).collect()
    } else {
        dir_names.iter().filter_map(build_child).collect()
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    // Fresh temp fixture dir (removing any stale one).
    fn temp_base(tag: &str) -> std::path::PathBuf {
        let base = std::env::temp_dir().join(format!("macdirstat-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        base
    }

    // FileTree::scan drains thread-local extension maps via a rayon broadcast,
    // so concurrent scans in one process can swap entries: serialize scans.
    // Poison-tolerant like production locks: one failing test must not cascade.
    static SCAN_LOCK: Mutex<()> = Mutex::new(());

    fn lock_scans() -> std::sync::MutexGuard<'static, ()> {
        SCAN_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    // Discard thread-local extension entries on this thread and all rayon
    // workers. Direct build_node_fd calls accumulate entries with no drain;
    // without this, the next FileTree::scan sweeps them into its own stats.
    // Takes the scan guard: draining is only meaningful under the lock.
    fn drain_local_ext_maps(_guard: &std::sync::MutexGuard<'static, ()>) {
        LOCAL_EXT_MAP.with(|m| {
            m.borrow_mut().clear();
        });
        rayon::broadcast(|_| {
            LOCAL_EXT_MAP.with(|m| {
                m.borrow_mut().clear();
            });
        });
    }

    // Find a direct child by name, panicking with its name when absent.
    fn find_child<'a>(root: &'a FileNode, name: &str) -> &'a FileNode {
        root.children
            .iter()
            .find(|c| &*c.name == name)
            .unwrap_or_else(|| panic!("missing child {name}"))
    }

    // Fixture with subdirs a/ and b/ (512B file each). The caller plays the
    // Data volume root; `a` is the mirror target with the given canonical.
    // Tags must stay distinct: temp dirs are shared per process. The scan
    // root is synthetic (lowercase by construction); only its prefix
    // relation to the canonical matters.
    fn mirror_fixture(tag: &str, canon: &str) -> (std::path::PathBuf, ScanCtx) {
        let base = temp_base(tag);
        std::fs::create_dir_all(base.join("a")).unwrap();
        std::fs::create_dir_all(base.join("b")).unwrap();
        std::fs::write(base.join("a/file.txt"), vec![b'x'; 512]).unwrap();
        std::fs::write(base.join("b/file.txt"), vec![b'y'; 512]).unwrap();
        let base_fd = getattrlistbulk::open_dir(&base);
        let base_id = dir_ident(base_fd);
        assert!(base_id.is_some());
        getattrlistbulk::close_dir(base_fd);
        let ctx = ScanCtx {
            visited: VisitedDirs::default(),
            firmlinks: [("a".into(), canon.into())].into_iter().collect(),
            data_root: base_id,
            scan_root: "/base".into(),
        };
        (base, ctx)
    }

    /// Scanning must not follow symlinks: a symlink to a dir stays a childless
    /// file node, its target is counted once, and a self-loop terminates.
    #[test]
    fn scan_does_not_follow_symlinks() {
        let base = temp_base("symlink-test");
        std::fs::create_dir_all(base.join("real")).unwrap();
        std::fs::write(base.join("real/file.txt"), vec![b'x'; 1024]).unwrap();
        symlink("real", base.join("link_to_real")).unwrap();
        symlink("real/file.txt", base.join("link_to_file")).unwrap();
        symlink("loop", base.join("loop")).unwrap();

        let _guard = lock_scans();
        let tree = FileTree::scan(&base);

        let link = find_child(&tree.root, "link_to_real");
        assert!(!link.is_dir, "symlink to a dir must not be a dir node");
        assert!(link.children.is_empty());
        let lf = find_child(&tree.root, "link_to_file");
        assert!(!lf.is_dir, "symlink to a file must not be a dir node");
        assert!(lf.children.is_empty());
        assert!(
            lf.size < find_child(&tree.root, "real").size,
            "file link reports link size"
        );
        let lp = find_child(&tree.root, "loop");
        assert!(!lp.is_dir, "symlink loop must not be a dir node");
        let real = find_child(&tree.root, "real");
        assert!(real.is_dir);
        assert_eq!(real.children.len(), 1);
        assert!(
            link.size < real.size,
            "symlink must report link size, not target size"
        );
        let sum: u64 = tree.root.children.iter().map(|c| c.size).sum();
        assert_eq!(tree.root.size, sum, "target must be counted once");
        assert_eq!(tree.root.file_count, 4, "real file + 3 links");

        let _ = std::fs::remove_dir_all(&base);
    }

    /// Same directory opened twice (e.g. firmlink mirror paths share dev+ino):
    /// the second claim must be rejected so its contents aren't counted twice.
    #[test]
    fn claim_dir_rejects_second_fd_to_same_dir() {
        let base = temp_base("visited-test");
        std::fs::create_dir_all(&base).unwrap();

        let visited = VisitedDirs::default();
        let fd1 = getattrlistbulk::open_dir(&base);
        assert!(fd1 >= 0);
        assert!(claim_ident(dir_ident(fd1), &visited));
        let fd2 = getattrlistbulk::open_dir(&base);
        assert!(fd2 >= 0);
        assert!(
            !claim_ident(dir_ident(fd2), &visited),
            "same dir via a second fd must be rejected"
        );
        getattrlistbulk::close_dir(fd1);
        getattrlistbulk::close_dir(fd2);

        let _ = std::fs::remove_dir_all(&base);
    }

    /// Distinct directories must never be pruned: no false-positive dedup.
    #[test]
    fn scan_keeps_distinct_directories() {
        let base = temp_base("distinct-test");
        std::fs::create_dir_all(base.join("a")).unwrap();
        std::fs::create_dir_all(base.join("b")).unwrap();
        std::fs::write(base.join("a/file.txt"), vec![b'x'; 512]).unwrap();
        std::fs::write(base.join("b/file.txt"), vec![b'y'; 512]).unwrap();
        std::fs::write(base.join("top.md"), vec![b'z'; 256]).unwrap();
        std::fs::write(base.join("README"), vec![b'r'; 128]).unwrap();

        let _guard = lock_scans();
        let mut tree = FileTree::scan(&base);

        let a = find_child(&tree.root, "a");
        let b = find_child(&tree.root, "b");
        assert!(a.is_dir && b.is_dir);
        assert_eq!(a.children.len(), 1);
        assert_eq!(b.children.len(), 1);
        assert_eq!(tree.root.dir_count, 3, "root + a + b");
        assert_eq!(tree.root.file_count, 4, "one file per dir + top-level");
        fn bucket(exts: &[(Box<str>, u64)], name: &str) -> Option<u64> {
            exts.iter().find(|(ext, _)| &**ext == name).map(|(_, n)| *n)
        }
        // Extension stats drain into the tree: both txt files, summed.
        // (Snapshot the sum: a/b borrow tree, blocking the &mut rebuild below.)
        let txt_total = a.size + b.size;
        assert_eq!(bucket(&tree.extensions, "txt"), Some(txt_total));
        // Root-level files are recorded on the scan thread, so this pins the
        // main-thread drain arm (deleting it drops the md entry silently).
        assert_eq!(bucket(&tree.extensions, "md"), Some(256));
        // The extensionless file must land in the "(no ext)" bucket on both
        // the scan path and the rebuild path (pins ext_bucket's sentinel
        // mapping at both call sites).
        assert_eq!(bucket(&tree.extensions, "(no ext)"), Some(128));
        tree.rebuild_extensions();
        assert_eq!(bucket(&tree.extensions, "txt"), Some(txt_total));
        assert_eq!(bucket(&tree.extensions, "md"), Some(256));
        assert_eq!(bucket(&tree.extensions, "(no ext)"), Some(128));

        let _ = std::fs::remove_dir_all(&base);
    }

    /// Mirror pruning: exact targets and anything beneath them are skipped
    /// when the canonical is in the scan; siblings, lookalike prefixes, and
    /// mirrors whose canonical is outside the scan are kept. Containment is
    /// component-wise: `/users2` is not under `/users`.
    #[test]
    fn firmlink_mirror_matching() {
        let ctx_for = |root: &str| ScanCtx {
            visited: VisitedDirs::default(),
            firmlinks: [
                ("Users".into(), "/users".into()),
                (
                    "System/Library/Caches".into(),
                    "/system/library/caches".into(),
                ),
                ("usr/local".into(), "/usr/local".into()),
                ("Other".into(), "/users2".into()),
            ]
            .into_iter()
            .collect(),
            data_root: None,
            scan_root: root.into(),
        };
        let root = ctx_for("/");
        assert!(is_mirror_in_scan("Users", &root));
        assert!(is_mirror_in_scan("Users/gregoreesmaa", &root));
        assert!(is_mirror_in_scan("System/Library/Caches", &root));
        assert!(!is_mirror_in_scan("System/Library", &root));
        assert!(!is_mirror_in_scan("System", &root));
        assert!(!is_mirror_in_scan("Users2", &root));
        assert!(!is_mirror_in_scan("Applications", &root));
        assert!(!is_mirror_in_scan("usr/libexec", &root));
        assert!(is_mirror_in_scan("usr/local/bin", &root));

        // Same mirrors, narrower scan: canonicals outside are kept.
        let sys = ctx_for("/system");
        assert!(!is_mirror_in_scan("Users", &sys));
        assert!(is_mirror_in_scan("System/Library/Caches", &sys));
        assert!(is_mirror_in_scan("System/Library/Caches/x", &sys));
        assert!(!is_mirror_in_scan("usr/local", &sys));

        // Canonical boundary even when the target matches textually.
        let user = ctx_for("/user");
        assert!(!is_mirror_in_scan("Users", &user));

        // Canon-side boundary: "/users2" is not under "/users".
        let users = ctx_for("/users");
        assert!(!is_mirror_in_scan("Other", &users));

        // Empty table prunes nothing.
        let empty = ScanCtx {
            visited: VisitedDirs::default(),
            firmlinks: HashMap::new(),
            data_root: None,
            scan_root: "/".into(),
        };
        assert!(!is_mirror_in_scan("Users", &empty));
    }

    /// End-to-end prune through real traversal: with a synthetic ctx treating
    /// the fixture root as the Data volume and `a` as a mirror target whose
    /// canonical is in the scan, `a` disappears while sibling `b` scans normally.
    #[test]
    fn firmlink_mirror_subtree_is_skipped() {
        let (base, ctx) = mirror_fixture("mirror-test", "/base/a");
        // Direct traversal writes thread-local ext maps with no drain and
        // runs on shared rayon workers: lock and drain so later scans
        // see clean maps.
        let _guard = lock_scans();
        let root_fd = getattrlistbulk::open_dir(&base);
        // The fixture root plays the Data volume: its Data-relative path is "".
        let node = build_node_fd(root_fd, "base".into(), &ctx, Some(""));
        getattrlistbulk::close_dir(root_fd);
        drain_local_ext_maps(&_guard);

        assert_eq!(node.children.len(), 1);
        assert_eq!(&*node.children[0].name, "b");
        assert_eq!(node.children[0].children.len(), 1);
        assert_eq!(node.file_count, 1, "only b's file is counted");
        // The pruned mirror is skipped before claiming: only b is visited,
        // and a's identity stays claimable.
        assert_eq!(ctx.visited.lock().expect("visited").len(), 1);
        let a_fd = getattrlistbulk::open_dir(&base.join("a"));
        assert!(claim_ident(dir_ident(a_fd), &ctx.visited));
        getattrlistbulk::close_dir(a_fd);

        let _ = std::fs::remove_dir_all(&base);
    }

    /// Same fixture, but the mirror's canonical is outside the scan:
    /// the mirror is the only view, so it must be kept (no undercount).
    #[test]
    fn firmlink_mirror_kept_when_canonical_outside_scan() {
        let (base, ctx) = mirror_fixture("mirror-kept-test", "/elsewhere/a");
        let _guard = lock_scans();
        let root_fd = getattrlistbulk::open_dir(&base);
        let node = build_node_fd(root_fd, "base".into(), &ctx, Some(""));
        getattrlistbulk::close_dir(root_fd);
        drain_local_ext_maps(&_guard);

        assert_eq!(node.children.len(), 2);
        assert_eq!(node.dir_count, 3, "root + a + b");
        assert_eq!(node.file_count, 2, "both files are counted");

        let _ = std::fs::remove_dir_all(&base);
    }

    /// The firmlink parser skips blanks, comments, and malformed lines,
    /// trims slashes, and lowercases canonicals.
    #[test]
    fn parse_firmlinks_edges() {
        let pairs = parse_firmlinks(
            "# comment\n\nUsers\tUsers\nopt/\t/opt/\nUSERS2\tUSERS2\nCanonDir\tTargetName\n\
             bad-line-no-tab\nempty-target\t\n\t\nSystem/Library/Caches\tSystem/Library/Caches\n",
        );
        assert_eq!(pairs.get("Users"), Some(&"/users".into()));
        assert_eq!(pairs.get("opt"), Some(&"/opt".into()));
        assert_eq!(pairs.get("USERS2"), Some(&"/users2".into()));
        // Asymmetric pair pins column order: target keys, canonical values.
        assert_eq!(pairs.get("TargetName"), Some(&"/canondir".into()));
        assert!(!pairs.keys().any(|k| &**k == "CanonDir"));
        assert_eq!(
            pairs.get("System/Library/Caches"),
            Some(&"/system/library/caches".into())
        );
        assert!(!pairs.keys().any(|k| &**k == "bad-line-no-tab"));
        assert!(!pairs.keys().any(|k| &**k == "empty-target"));
        assert_eq!(pairs.len(), 5);
        assert!(parse_firmlinks("").is_empty());
    }

    /// The real firmlink table parses to the expected pairs (macOS only).
    #[test]
    fn firmlink_table_loads() {
        if !std::path::Path::new("/usr/share/firmlinks").exists() {
            eprintln!("skipping firmlink table test: not macOS");
            return; // not macOS — nothing to parse
        }
        let pairs = load_firmlinks();
        assert_eq!(pairs.get("Users"), Some(&"/users".into()));
        assert_eq!(
            pairs.get("System/Library/Caches"),
            Some(&"/system/library/caches".into())
        );
        for (target, canon) in pairs.iter() {
            assert!(!target.is_empty());
            assert!(!target.starts_with('/'));
            assert!(canon.starts_with('/'), "canonical must be absolute");
        }
    }

    /// Scan-context wiring: the root is canonicalized once, and the Data
    /// identity is tracked exactly when the table loaded.
    #[test]
    fn scan_ctx_wiring() {
        let base = temp_base("ctx-test");
        std::fs::create_dir_all(&base).unwrap();

        let ctx = make_scan_ctx(&base);
        let expect = std::fs::canonicalize(&base)
            .unwrap()
            .to_string_lossy()
            .to_lowercase();
        assert_eq!(&*ctx.scan_root, expect.as_str());
        assert_eq!(ctx.data_root.is_some(), !ctx.firmlinks.is_empty());

        let _ = std::fs::remove_dir_all(&base);
    }

    /// Scanning a missing root yields an empty tree, not a panic.
    /// (Unreadable subtrees behave the same: empty dir node, kept in place.)
    #[test]
    fn scan_of_missing_root_is_empty() {
        let missing = temp_base("missing-test").join("nope");
        let _guard = lock_scans();
        let tree = FileTree::scan(&missing);
        assert!(tree.root.children.is_empty());
        assert_eq!(tree.root.size, 0);
        assert_eq!(tree.root.file_count, 0);
        assert_eq!(tree.root.dir_count, 1);
    }

    /// An unreadable subdir scans as an empty dir without panicking.
    /// (Unreadable content stays visible in place — never silently dropped.)
    #[test]
    fn scan_tolerates_unreadable_subdir() {
        if unsafe { libc::geteuid() } == 0 {
            eprintln!("skipping unreadable-subdir test: running as root");
            return; // root reads anything; permission bit is no barrier
        }
        // Restores access on drop so cleanup works even if an assert panics.
        struct RestorePerms<'a> {
            path: &'a std::path::Path,
        }
        impl Drop for RestorePerms<'_> {
            fn drop(&mut self) {
                let _ = std::fs::set_permissions(self.path, std::fs::Permissions::from_mode(0o755));
            }
        }
        let base = temp_base("unreadable-test");
        let locked_path = base.join("locked");
        std::fs::create_dir_all(&locked_path).unwrap();
        std::fs::create_dir_all(base.join("open")).unwrap();
        std::fs::write(locked_path.join("file.txt"), vec![b'x'; 512]).unwrap();
        std::fs::write(base.join("open/file.txt"), vec![b'y'; 512]).unwrap();
        std::fs::set_permissions(&locked_path, std::fs::Permissions::from_mode(0o000)).unwrap();
        let _restore = RestorePerms { path: &locked_path };

        let _guard = lock_scans();
        let tree = FileTree::scan(&base);

        let locked = find_child(&tree.root, "locked");
        assert!(locked.is_dir && locked.children.is_empty());
        assert_eq!(find_child(&tree.root, "open").children.len(), 1);
        assert_eq!(tree.root.file_count, 1, "only the readable file counts");

        // Restore before removing: removal needs write access to locked/.
        // (The RAII guard only covers panic paths from here on.)
        std::fs::set_permissions(&locked_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Data-relative path tracking: the Data root itself, plain join,
    /// empty-parent join, and the outside-Data case.
    #[test]
    fn child_data_rel_tracking() {
        let ctx = ScanCtx {
            visited: VisitedDirs::default(),
            firmlinks: HashMap::new(),
            data_root: Some((9, 9)),
            scan_root: "/".into(),
        };
        assert_eq!(
            child_data_rel("x", Some((9, 9)), None, &ctx),
            Some(String::new())
        );
        assert_eq!(child_data_rel("x", Some((1, 2)), None, &ctx), None);
        assert_eq!(
            child_data_rel("c", Some((1, 2)), Some("a/b"), &ctx),
            Some("a/b/c".into())
        );
        assert_eq!(
            child_data_rel("c", Some((1, 2)), Some(""), &ctx),
            Some("c".into())
        );
        // The scan root itself: set only when the root IS the Data volume.
        assert_eq!(
            child_data_rel("", Some((9, 9)), None, &ctx),
            Some(String::new())
        );
        assert_eq!(child_data_rel("", Some((1, 2)), None, &ctx), None);
        assert_eq!(child_data_rel("", None, None, &ctx), None);
    }

    /// An explicitly scanned root may be a symlink: open_dir follows it
    /// while traversal below still refuses links.
    #[test]
    fn scan_through_symlinked_root() {
        let base = temp_base("symlink-root-test");
        std::fs::create_dir_all(base.join("real")).unwrap();
        std::fs::write(base.join("real/file.txt"), vec![b'x'; 512]).unwrap();
        symlink("real", base.join("linkroot")).unwrap();

        let _guard = lock_scans();
        let tree = FileTree::scan(&base.join("linkroot"));

        assert_eq!(tree.root.children.len(), 1);
        assert_eq!(&*tree.root.children[0].name, "file.txt");
        assert_eq!(tree.root.file_count, 1);

        let _ = std::fs::remove_dir_all(&base);
    }
}
