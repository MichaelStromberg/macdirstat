//! Benchmark: `cargo run --release --example bench_scan -- <path> [passes]`
use std::time::Instant;

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| ".".into());
    let passes: usize = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(3);
    let mut times = Vec::new();
    for i in 0..=passes {
        let t = Instant::now();
        let tree = macdirstat::model::tree::FileTree::scan(std::path::Path::new(&path));
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        println!(
            "pass {i}{}: {ms:.0}ms  files={} dirs={} size={}",
            if i == 0 { " (warmup)" } else { "" },
            tree.root.file_count,
            tree.root.dir_count,
            macdirstat::format_size(tree.root.size)
        );
        if i > 0 {
            times.push(ms);
        }
    }
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!("median: {:.0}ms", times[times.len() / 2]);

    // Cache round-trip timing (written to ~/Library/Caches/macdirstat).
    let tree = macdirstat::model::tree::FileTree::scan(std::path::Path::new(&path));
    let t = Instant::now();
    let encoded = macdirstat::cache::encode(&tree);
    let encode_ms = t.elapsed().as_secs_f64() * 1000.0;
    let t = Instant::now();
    macdirstat::cache::write(&encoded).expect("cache write");
    let write_ms = t.elapsed().as_secs_f64() * 1000.0;
    let t = Instant::now();
    let loaded =
        macdirstat::cache::load(std::path::Path::new(&tree.root_path)).expect("cache load");
    let load_ms = t.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(loaded.tree.root.size, tree.root.size);
    assert_eq!(loaded.tree.root.file_count, tree.root.file_count);
    println!("cache: encode {encode_ms:.0}ms, compress+write {write_ms:.0}ms, load {load_ms:.0}ms");
}
