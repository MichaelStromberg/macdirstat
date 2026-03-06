use std::path::Path;

use super::jwalk_scan::ScanResult;

/// Sequential directory walk using walkdir.
pub fn scan(root: &Path) -> ScanResult {
    let mut total_size: u64 = 0;
    let mut file_count: u64 = 0;
    let mut dir_count: u64 = 0;

    for entry in walkdir::WalkDir::new(root) {
        let Ok(entry) = entry else { continue };
        if entry.file_type().is_file() {
            if let Ok(meta) = entry.metadata() {
                total_size += meta.len();
            }
            file_count += 1;
        } else if entry.file_type().is_dir() {
            dir_count += 1;
        }
    }

    ScanResult {
        total_size,
        file_count,
        dir_count,
    }
}
