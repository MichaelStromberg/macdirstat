use std::path::Path;

use super::jwalk_scan::ScanResult;

/// Parallel walk using the `ignore` crate (from ripgrep).
/// We disable all ignore rules so it scans everything.
pub fn scan(root: &Path) -> ScanResult {
    let mut total_size: u64 = 0;
    let mut file_count: u64 = 0;
    let mut dir_count: u64 = 0;

    let walker = ignore::WalkBuilder::new(root)
        .hidden(false)
        .ignore(false)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .sort_by_file_path(|a, b| a.cmp(b))
        .build();

    for entry in walker {
        let Ok(entry) = entry else { continue };
        let Some(ft) = entry.file_type() else {
            continue;
        };
        if ft.is_file() {
            if let Ok(meta) = entry.metadata() {
                total_size += meta.len();
            }
            file_count += 1;
        } else if ft.is_dir() {
            dir_count += 1;
        }
    }

    ScanResult {
        total_size,
        file_count,
        dir_count,
    }
}
