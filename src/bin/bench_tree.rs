//! Benchmark harness for FileTree::scan — measures full scan + tree build.
//!
//! Usage: cargo run --release --bin bench_tree [path] [--label <tag>]

use std::path::Path;
use std::time::Instant;

use macdirstat::model::tree::FileTree;

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

fn main() {
    let args: Vec<String> = std::env::args().collect();

    let mut path_str = "/Users/michael/source".to_string();
    let mut label = String::new();
    let mut i = 1;
    while i < args.len() {
        if args[i] == "--label" {
            if i + 1 < args.len() {
                label = args[i + 1].clone();
                i += 2;
            } else {
                eprintln!("--label requires a value");
                std::process::exit(1);
            }
        } else {
            path_str = args[i].clone();
            i += 1;
        }
    }

    let path = Path::new(&path_str);
    if !path.exists() {
        eprintln!("Error: path '{}' does not exist.", path.display());
        std::process::exit(1);
    }

    let label_str = if label.is_empty() {
        String::new()
    } else {
        format!(" [{}]", label)
    };

    println!("=== FileTree::scan Benchmark{} ===", label_str);
    println!("Path: {}", path.display());
    println!();

    // Warm-up pass
    print!("Warm-up... ");
    let start = Instant::now();
    let tree = FileTree::scan(path);
    let warmup_ms = start.elapsed().as_secs_f64() * 1000.0;
    println!(
        "{:.0}ms ({} files, {} dirs, {})",
        warmup_ms,
        tree.root.file_count,
        tree.root.dir_count,
        format_size(tree.root.size)
    );
    drop(tree);

    // 3 timed passes
    const PASSES: usize = 3;
    let mut times = Vec::with_capacity(PASSES);

    for pass in 1..=PASSES {
        let start = Instant::now();
        let tree = FileTree::scan(path);
        let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
        times.push(elapsed_ms);
        println!(
            "Pass {}: {:.0}ms ({} files, {} dirs, {})",
            pass,
            elapsed_ms,
            tree.root.file_count,
            tree.root.dir_count,
            format_size(tree.root.size)
        );
        drop(tree);
    }

    times.sort_by(|a, b| a.total_cmp(b));
    let median = times[PASSES / 2];

    println!();
    println!("Results{}:", label_str);
    println!(
        "  Times: {}",
        times
            .iter()
            .map(|t| format!("{:.0}ms", t))
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!("  Median: {:.0}ms", median);
}
