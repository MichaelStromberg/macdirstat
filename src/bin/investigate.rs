//! Comprehensive pre-implementation investigation binary for MacDirStat.
//!
//! Usage: cargo run --bin investigate <subcommand> [args...]
//!
//! Subcommands:
//!   cold-cache   - Time each scanner once against a path (run `sudo purge` first)
//!   real-world   - Scan several real macOS directories with all scanners
//!   memory       - Measure peak RSS when building an in-memory tree
//!   permissions  - Investigate how scanners handle permission errors
//!   hardlinks    - Test hardlink, firmlink, and symlink handling

use std::path::{Path, PathBuf};
use std::time::Instant;

use macdirstat::scan::getattrlistbulk;
use macdirstat::scan::ignore_scan;
use macdirstat::scan::jwalk_scan;
use macdirstat::scan::jwalk_scan::ScanResult;
use macdirstat::scan::std_readdir;
use macdirstat::scan::walkdir_scan;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;
    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

fn print_row(cols: &[&str], widths: &[usize]) {
    for (i, col) in cols.iter().enumerate() {
        let w = widths.get(i).copied().unwrap_or(12);
        print!("{:<width$}", col, width = w);
    }
    println!();
}

fn print_separator(widths: &[usize]) {
    for w in widths {
        print!("{:-<width$}", "", width = *w);
    }
    println!();
}

/// Run a scanner and return (ScanResult, elapsed_ms).
fn timed_scan<F>(f: F) -> (ScanResult, f64)
where
    F: FnOnce() -> ScanResult,
{
    let start = Instant::now();
    let result = f();
    let elapsed = start.elapsed().as_secs_f64() * 1000.0;
    (result, elapsed)
}

type ScannerFn = fn(&Path) -> ScanResult;

fn all_scanners() -> Vec<(&'static str, ScannerFn)> {
    vec![
        ("std_readdir", std_readdir::scan as ScannerFn),
        ("walkdir", walkdir_scan::scan as ScannerFn),
        ("jwalk", jwalk_scan::scan as ScannerFn),
        ("ignore", ignore_scan::scan as ScannerFn),
        ("getattrlistbulk", getattrlistbulk::scan as ScannerFn),
        (
            "getattrlistbulk_par",
            getattrlistbulk::scan_parallel as ScannerFn,
        ),
    ]
}

// ---------------------------------------------------------------------------
// Subcommand: cold-cache
// ---------------------------------------------------------------------------

fn cmd_cold_cache(args: &[String]) {
    let default_path = "/usr".to_string();
    let path_str = args.first().unwrap_or(&default_path);
    let path = Path::new(path_str);

    println!("=== Cold-Cache Scanner Timing ===");
    println!();
    println!("IMPORTANT: For accurate cold-cache results, run this command first:");
    println!("  sudo purge");
    println!();
    println!("Target path: {}", path.display());
    println!();

    if !path.exists() {
        eprintln!("Error: path '{}' does not exist.", path.display());
        return;
    }

    let widths = [24, 14, 14, 14, 14];
    print_row(
        &["Scanner", "Files", "Dirs", "Total Size", "Time (ms)"],
        &widths,
    );
    print_separator(&widths);

    for (name, scanner) in all_scanners() {
        let (result, elapsed) = timed_scan(|| scanner(path));
        print_row(
            &[
                name,
                &result.file_count.to_string(),
                &result.dir_count.to_string(),
                &format_size(result.total_size),
                &format!("{:.1}", elapsed),
            ],
            &widths,
        );
    }
}

// ---------------------------------------------------------------------------
// Subcommand: real-world
// ---------------------------------------------------------------------------

fn cmd_real_world(_args: &[String]) {
    println!("=== Real-World Directory Scanning ===");
    println!();

    let home = std::env::var("HOME").unwrap_or_else(|_| "/Users/nobody".to_string());

    let mut dirs: Vec<PathBuf> = vec![
        PathBuf::from("/usr"),
        PathBuf::from("/Applications"),
        PathBuf::from(&home).join("Library"),
    ];

    // /System -- only add if accessible
    let system_path = PathBuf::from("/System");
    if system_path.exists() {
        dirs.push(system_path);
    }

    // data/bench_tree -- only add if it exists
    let bench_tree = PathBuf::from("data/bench_tree");
    if bench_tree.exists() {
        dirs.push(bench_tree);
    }

    let widths = [32, 14, 14, 24, 14];
    print_row(
        &["Directory", "Files", "Dirs", "Method", "Time (ms)"],
        &widths,
    );
    print_separator(&widths);

    for dir in &dirs {
        if !dir.exists() {
            println!(
                "{:<width$}(path does not exist, skipping)",
                dir.display(),
                width = widths[0]
            );
            continue;
        }

        for (name, scanner) in all_scanners() {
            let (result, elapsed) = timed_scan(|| scanner(dir));
            print_row(
                &[
                    &dir.display().to_string(),
                    &result.file_count.to_string(),
                    &result.dir_count.to_string(),
                    name,
                    &format!("{:.1}", elapsed),
                ],
                &widths,
            );
        }
        println!();
    }
}

// ---------------------------------------------------------------------------
// Subcommand: memory
// ---------------------------------------------------------------------------

struct TreeNode {
    #[allow(dead_code)]
    name: String,
    #[allow(dead_code)]
    size: u64,
    children: Vec<TreeNode>,
}

struct CompactNode {
    #[allow(dead_code)]
    name: Box<str>,
    #[allow(dead_code)]
    size: u64,
    children: Box<[CompactNode]>,
}

fn build_tree(dir: &Path) -> TreeNode {
    let mut children = Vec::new();

    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => {
            return TreeNode {
                name: dir
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                size: 0,
                children: Vec::new(),
            };
        }
    };

    let mut total_size: u64 = 0;

    for entry in entries {
        let Ok(entry) = entry else { continue };
        let Ok(ft) = entry.file_type() else { continue };

        if ft.is_file() {
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            total_size += size;
            children.push(TreeNode {
                name: entry.file_name().to_string_lossy().into_owned(),
                size,
                children: Vec::new(),
            });
        } else if ft.is_dir() {
            let child = build_tree(&entry.path());
            children.push(child);
        }
    }

    TreeNode {
        name: dir
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
        size: total_size,
        children,
    }
}

fn build_compact_tree(dir: &Path) -> CompactNode {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => {
            return CompactNode {
                name: dir
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default()
                    .into_boxed_str(),
                size: 0,
                children: Box::new([]),
            };
        }
    };

    let mut children = Vec::new();
    let mut total_size: u64 = 0;

    for entry in entries {
        let Ok(entry) = entry else { continue };
        let Ok(ft) = entry.file_type() else { continue };

        if ft.is_file() {
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            total_size += size;
            children.push(CompactNode {
                name: entry
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
                    .into_boxed_str(),
                size,
                children: Box::new([]),
            });
        } else if ft.is_dir() {
            let child = build_compact_tree(&entry.path());
            children.push(child);
        }
    }

    CompactNode {
        name: dir
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
            .into_boxed_str(),
        size: total_size,
        children: children.into_boxed_slice(),
    }
}

fn count_nodes(node: &TreeNode) -> u64 {
    1 + node.children.iter().map(count_nodes).sum::<u64>()
}

fn count_compact_nodes(node: &CompactNode) -> u64 {
    1 + node.children.iter().map(count_compact_nodes).sum::<u64>()
}

fn get_peak_rss_bytes() -> u64 {
    unsafe {
        let mut usage: libc::rusage = std::mem::zeroed();
        libc::getrusage(libc::RUSAGE_SELF, &mut usage);
        // On macOS ru_maxrss is in bytes
        usage.ru_maxrss as u64
    }
}

fn cmd_memory(args: &[String]) {
    let default_path = "data/bench_tree".to_string();
    let path_str = args.first().unwrap_or(&default_path);
    let path = Path::new(path_str);

    println!("=== Memory Usage Investigation ===");
    println!();
    println!("Target path: {}", path.display());
    println!();

    if !path.exists() {
        eprintln!("Error: path '{}' does not exist.", path.display());
        eprintln!(
            "Hint: you may need to generate data/bench_tree first, or provide a different path."
        );
        return;
    }

    // --- TreeNode (Vec-based) ---
    println!("--- TreeNode (Vec<TreeNode> children) ---");
    let rss_before = get_peak_rss_bytes();
    let start = Instant::now();
    let tree = build_tree(path);
    let elapsed = start.elapsed().as_secs_f64() * 1000.0;
    let rss_after = get_peak_rss_bytes();
    let node_count = count_nodes(&tree);

    println!("  Build time:        {:.1} ms", elapsed);
    println!("  Total nodes:       {}", node_count);
    println!("  Peak RSS before:   {}", format_size(rss_before));
    println!("  Peak RSS after:    {}", format_size(rss_after));
    let rss_diff = rss_after.saturating_sub(rss_before);
    println!("  RSS delta:         {}", format_size(rss_diff));
    if node_count > 0 {
        println!(
            "  Est. bytes/node:   {} (RSS delta / nodes)",
            if rss_diff > 0 {
                format!("{}", rss_diff / node_count)
            } else {
                "N/A (peak RSS did not increase, tree may fit in prior allocation)".to_string()
            }
        );
    }
    println!(
        "  sizeof(TreeNode):  {} bytes",
        std::mem::size_of::<TreeNode>()
    );

    // Drop the tree to free memory (though peak RSS won't decrease)
    drop(tree);
    println!();

    // --- CompactNode (Box-based) ---
    println!("--- CompactNode (Box<[CompactNode]> children) ---");
    let rss_before2 = get_peak_rss_bytes();
    let start2 = Instant::now();
    let compact_tree = build_compact_tree(path);
    let elapsed2 = start2.elapsed().as_secs_f64() * 1000.0;
    let rss_after2 = get_peak_rss_bytes();
    let compact_count = count_compact_nodes(&compact_tree);

    println!("  Build time:        {:.1} ms", elapsed2);
    println!("  Total nodes:       {}", compact_count);
    println!("  Peak RSS before:   {}", format_size(rss_before2));
    println!("  Peak RSS after:    {}", format_size(rss_after2));
    let rss_diff2 = rss_after2.saturating_sub(rss_before2);
    println!("  RSS delta:         {}", format_size(rss_diff2));
    if compact_count > 0 {
        println!(
            "  Est. bytes/node:   {} (RSS delta / nodes)",
            if rss_diff2 > 0 {
                format!("{}", rss_diff2 / compact_count)
            } else {
                "N/A (peak RSS did not increase)".to_string()
            }
        );
    }
    println!(
        "  sizeof(CompactNode): {} bytes",
        std::mem::size_of::<CompactNode>()
    );

    drop(compact_tree);
    println!();

    // --- Summary ---
    println!("--- Struct Size Comparison ---");
    println!(
        "  sizeof(TreeNode):    {} bytes",
        std::mem::size_of::<TreeNode>()
    );
    println!(
        "  sizeof(CompactNode): {} bytes",
        std::mem::size_of::<CompactNode>()
    );
    let savings =
        std::mem::size_of::<TreeNode>() as i64 - std::mem::size_of::<CompactNode>() as i64;
    println!(
        "  Per-struct savings:  {} bytes (compact is {})",
        savings.abs(),
        if savings > 0 { "smaller" } else { "larger" }
    );
}

// ---------------------------------------------------------------------------
// Subcommand: permissions
// ---------------------------------------------------------------------------

struct PermissionScanResult {
    files: u64,
    dirs: u64,
    permission_errors: u64,
}

fn scan_with_permission_tracking(dir: &Path) -> PermissionScanResult {
    let mut result = PermissionScanResult {
        files: 0,
        dirs: 0,
        permission_errors: 0,
    };
    scan_permissions_recursive(dir, &mut result);
    result
}

fn scan_permissions_recursive(dir: &Path, result: &mut PermissionScanResult) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            if e.kind() == std::io::ErrorKind::PermissionDenied {
                result.permission_errors += 1;
            }
            return;
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                if e.kind() == std::io::ErrorKind::PermissionDenied {
                    result.permission_errors += 1;
                }
                continue;
            }
        };

        let ft = match entry.file_type() {
            Ok(ft) => ft,
            Err(e) => {
                if e.kind() == std::io::ErrorKind::PermissionDenied {
                    result.permission_errors += 1;
                }
                continue;
            }
        };

        if ft.is_file() {
            result.files += 1;
        } else if ft.is_dir() {
            result.dirs += 1;
            scan_permissions_recursive(&entry.path(), result);
        }
    }
}

fn cmd_permissions(_args: &[String]) {
    println!("=== Permission Handling Investigation ===");
    println!();

    let home = std::env::var("HOME").unwrap_or_else(|_| "/Users/nobody".to_string());

    let dirs: Vec<PathBuf> = vec![
        PathBuf::from("/System"),
        PathBuf::from("/private/var"),
        PathBuf::from(&home).join("Library"),
        PathBuf::from("/Library"),
    ];

    let widths = [28, 12, 12, 18, 24];
    print_row(
        &["Path", "Files", "Dirs", "Perm Errors", "Scanner"],
        &widths,
    );
    print_separator(&widths);

    for dir in &dirs {
        if !dir.exists() {
            println!(
                "{:<width$}(does not exist, skipping)",
                dir.display(),
                width = widths[0]
            );
            continue;
        }

        // std::fs::read_dir with explicit permission tracking
        let perm_result = scan_with_permission_tracking(dir);
        print_row(
            &[
                &dir.display().to_string(),
                &perm_result.files.to_string(),
                &perm_result.dirs.to_string(),
                &perm_result.permission_errors.to_string(),
                "std_readdir (tracked)",
            ],
            &widths,
        );

        // getattrlistbulk
        let gattrb = getattrlistbulk::scan(dir);
        print_row(
            &[
                &dir.display().to_string(),
                &gattrb.file_count.to_string(),
                &gattrb.dir_count.to_string(),
                "N/A (silent)",
                "getattrlistbulk",
            ],
            &widths,
        );

        // jwalk
        let jw = jwalk_scan::scan(dir);
        print_row(
            &[
                &dir.display().to_string(),
                &jw.file_count.to_string(),
                &jw.dir_count.to_string(),
                "N/A (skips)",
                "jwalk",
            ],
            &widths,
        );

        println!();
    }

    println!("Notes:");
    println!("  - 'Perm Errors' counts PermissionDenied errors encountered during traversal.");
    println!("  - getattrlistbulk silently skips directories it cannot open (fd < 0).");
    println!("  - jwalk skips entries where the iterator yields Err.");
    println!("  - Differences in file/dir counts between scanners may indicate silent skipping.");
}

// ---------------------------------------------------------------------------
// Subcommand: hardlinks
// ---------------------------------------------------------------------------

fn cmd_hardlinks(_args: &[String]) {
    println!("=== Hardlink / Firmlink / Symlink Investigation ===");
    println!();

    let tmp_base = std::env::temp_dir().join("macdirstat_investigate_hardlinks");

    // Clean up any previous run
    let _ = std::fs::remove_dir_all(&tmp_base);
    if let Err(e) = std::fs::create_dir_all(&tmp_base) {
        eprintln!("Error creating temp dir: {}", e);
        return;
    }

    // --- Part 1: Hardlinks ---
    println!("--- Part 1: Hardlink counting ---");
    println!();

    let original = tmp_base.join("original.txt");
    std::fs::write(&original, "hello hardlinks").unwrap_or_else(|e| {
        eprintln!("Error writing original file: {}", e);
    });

    let link1 = tmp_base.join("hardlink1.txt");
    let link2 = tmp_base.join("hardlink2.txt");

    if let Err(e) = std::fs::hard_link(&original, &link1) {
        eprintln!("Error creating hardlink1: {}", e);
    }
    if let Err(e) = std::fs::hard_link(&original, &link2) {
        eprintln!("Error creating hardlink2: {}", e);
    }

    println!("Created 1 file + 2 hardlinks in {}", tmp_base.display());
    println!("All three names point to the same inode.");
    println!();

    let widths = [24, 10, 10, 16];
    print_row(&["Scanner", "Files", "Dirs", "Total Size"], &widths);
    print_separator(&widths);

    for (name, scanner) in all_scanners() {
        let result = scanner(&tmp_base);
        print_row(
            &[
                name,
                &result.file_count.to_string(),
                &result.dir_count.to_string(),
                &format_size(result.total_size),
            ],
            &widths,
        );
    }

    println!();
    println!("Expected: 3 files counted (each hardlink is a separate dir entry).");
    println!("Total size may triple-count the same data on disk.");
    println!();

    // --- Part 2: Firmlinks ---
    println!("--- Part 2: Firmlink comparison ---");
    println!();

    let firmlink_paths: Vec<(&str, &str)> = vec![
        ("/usr", "/System/Volumes/Data/usr"),
        ("/Applications", "/System/Volumes/Data/Applications"),
    ];

    let widths2 = [40, 12, 12, 16];
    print_row(&["Path", "Files", "Dirs", "Scanner"], &widths2);
    print_separator(&widths2);

    for (visible, data_vol) in &firmlink_paths {
        let vp = Path::new(visible);
        let dp = Path::new(data_vol);

        if vp.exists() {
            let result = getattrlistbulk::scan(vp);
            print_row(
                &[
                    visible,
                    &result.file_count.to_string(),
                    &result.dir_count.to_string(),
                    "getattrlistbulk",
                ],
                &widths2,
            );
        } else {
            println!("{:<width$}(not accessible)", visible, width = widths2[0]);
        }

        if dp.exists() {
            let result = getattrlistbulk::scan(dp);
            print_row(
                &[
                    data_vol,
                    &result.file_count.to_string(),
                    &result.dir_count.to_string(),
                    "getattrlistbulk",
                ],
                &widths2,
            );
        } else {
            println!("{:<width$}(not accessible)", data_vol, width = widths2[0]);
        }

        println!();
    }

    println!("If file counts match between a path and its /System/Volumes/Data counterpart,");
    println!("they are firmlinked (same underlying directory).");
    println!();

    // --- Part 3: Symlinks ---
    println!("--- Part 3: Symlink handling ---");
    println!();

    let symlink_dir = tmp_base.join("symlink_test");
    let _ = std::fs::create_dir_all(&symlink_dir);

    let real_file = symlink_dir.join("real.txt");
    std::fs::write(&real_file, "real file content").unwrap_or_else(|e| {
        eprintln!("Error writing real file: {}", e);
    });

    let sym_file = symlink_dir.join("sym.txt");
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        if let Err(e) = symlink(&real_file, &sym_file) {
            eprintln!("Error creating symlink: {}", e);
        }
    }

    // Also create a symlink to a directory
    let real_subdir = symlink_dir.join("real_subdir");
    let _ = std::fs::create_dir_all(&real_subdir);
    std::fs::write(real_subdir.join("inner.txt"), "inner").unwrap_or_else(|e| {
        eprintln!("Error writing inner file: {}", e);
    });

    let sym_subdir = symlink_dir.join("sym_subdir");
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        if let Err(e) = symlink(&real_subdir, &sym_subdir) {
            eprintln!("Error creating dir symlink: {}", e);
        }
    }

    println!("Created test structure in {}", symlink_dir.display());
    println!("  real.txt          - regular file");
    println!("  sym.txt           - symlink -> real.txt");
    println!("  real_subdir/      - directory with inner.txt");
    println!("  sym_subdir        - symlink -> real_subdir/");
    println!();
    println!("If a scanner follows symlinks, it will count inner.txt twice");
    println!("and report more files/dirs than one that does not follow.");
    println!();

    let widths3 = [24, 10, 10];
    print_row(&["Scanner", "Files", "Dirs"], &widths3);
    print_separator(&widths3);

    for (name, scanner) in all_scanners() {
        let result = scanner(&symlink_dir);
        print_row(
            &[
                name,
                &result.file_count.to_string(),
                &result.dir_count.to_string(),
            ],
            &widths3,
        );
    }

    println!();
    println!("Scanners that do NOT follow symlinks: files=2, dirs=1 (root + real_subdir)");
    println!("Scanners that DO follow symlinks:     files=3+, dirs=2+");

    // Cleanup
    println!();
    println!("Cleaning up temp directory...");
    if let Err(e) = std::fs::remove_dir_all(&tmp_base) {
        eprintln!("Warning: could not clean up {}: {}", tmp_base.display(), e);
    } else {
        println!("Done.");
    }
}

// ---------------------------------------------------------------------------
// Help
// ---------------------------------------------------------------------------

fn print_help() {
    println!("MacDirStat Investigation Tool");
    println!();
    println!("Usage: cargo run --bin investigate <subcommand> [args...]");
    println!();
    println!("Subcommands:");
    println!("  cold-cache [path]   Time each scanner once after cache purge (default: /usr)");
    println!("  real-world          Scan several real macOS directories with all scanners");
    println!(
        "  memory [path]       Measure peak RSS building in-memory tree (default: data/bench_tree)"
    );
    println!("  permissions         Investigate how scanners handle permission errors");
    println!("  hardlinks           Test hardlink, firmlink, and symlink handling");
    println!("  help                Show this help message");
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(|s| s.as_str()).unwrap_or("help");
    let rest = if args.len() > 2 {
        args[2..].to_vec()
    } else {
        Vec::new()
    };

    match cmd {
        "cold-cache" => cmd_cold_cache(&rest),
        "real-world" => cmd_real_world(&rest),
        "memory" => cmd_memory(&rest),
        "permissions" => cmd_permissions(&rest),
        "hardlinks" => cmd_hardlinks(&rest),
        _ => print_help(),
    }
}
