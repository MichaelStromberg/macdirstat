use std::fs;
use std::path::Path;

use super::jwalk_scan::ScanResult;

/// Baseline: recursive std::fs::read_dir + metadata() (uses fstatat on macOS).
pub fn scan(root: &Path) -> ScanResult {
    let mut total_size: u64 = 0;
    let mut file_count: u64 = 0;
    let mut dir_count: u64 = 0;

    scan_recursive(root, &mut total_size, &mut file_count, &mut dir_count);

    ScanResult {
        total_size,
        file_count,
        dir_count,
    }
}

fn scan_recursive(dir: &Path, total_size: &mut u64, file_count: &mut u64, dir_count: &mut u64) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries {
        let Ok(entry) = entry else { continue };
        let Ok(ft) = entry.file_type() else { continue };

        if ft.is_file() {
            if let Ok(meta) = entry.metadata() {
                *total_size += meta.len();
            }
            *file_count += 1;
        } else if ft.is_dir() {
            *dir_count += 1;
            scan_recursive(&entry.path(), total_size, file_count, dir_count);
        }
    }
}
