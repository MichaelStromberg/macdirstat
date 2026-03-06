use std::path::Path;

pub struct ScanResult {
    pub total_size: u64,
    pub file_count: u64,
    pub dir_count: u64,
}

/// Parallel directory walk using jwalk (rayon-based work stealing).
pub fn scan(root: &Path) -> ScanResult {
    let mut total_size: u64 = 0;
    let mut file_count: u64 = 0;
    let mut dir_count: u64 = 0;

    for entry in jwalk::WalkDir::new(root).skip_hidden(false).sort(false) {
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
