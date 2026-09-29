pub mod app;
pub mod cache;
pub mod model;
#[cfg(target_os = "macos")]
pub(crate) mod objc_ffi;
pub mod scan;
pub mod ui;

/// Format a byte count as a human-readable string (e.g., "1.2 GB", "340.5 MB").
pub fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;
    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

/// Show the given file or folder selected in a Finder window.
pub fn reveal_in_finder(path: &std::path::Path) {
    if let Err(e) = std::process::Command::new("open")
        .arg("-R")
        .arg(path)
        .spawn()
    {
        eprintln!("Failed to reveal {:?} in Finder: {}", path, e);
    }
}
