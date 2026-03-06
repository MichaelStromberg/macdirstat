# Disk Scanning Research: Fast File System Traversal on macOS

## 1. WizTree's MFT Approach (Windows/NTFS)

WizTree achieves its extraordinary speed (46x faster than traditional analyzers) by reading the NTFS **Master File Table (MFT)** directly from disk, completely bypassing Windows filesystem APIs.

### How It Works
- Every NTFS volume has a hidden `$MFT` file that records the name, size, and location of every file
- WizTree parses the MFT binary format directly instead of traversing directories one-by-one
- Requires Administrator privileges to access raw disk sectors
- Falls back to standard API enumeration for non-NTFS volumes (FAT, exFAT, network shares)

### WinDirStat's Dual Scanner Architecture
The windirstat codebase includes two scanner implementations:
- **FinderBasic** (`FinderBasic.cpp`): Uses `NtQueryDirectoryFile` with a 4MB buffer for batch directory enumeration
- **FinderNtfs** (`FinderNtfs.cpp`): Direct NTFS MFT parsing, similar to WizTree's approach. Parses `FILE_RECORD` structures and attribute types (StandardInformation 0x10, FileName 0x30, Data 0x80, ReparsePoint 0xC0)

### References
- [WizTree - About](https://diskanalyzer.com/about)
- [WizTree: Fast NTFS Disk Scan](https://windowsforum.com/threads/wiztree-fast-ntfs-disk-scan-to-reveal-hidden-ssd-space-hoggers-in-minutes.386938/)

---

## 2. macOS/APFS: No Direct MFT Equivalent

**APFS does not have a centralized metadata structure like NTFS's MFT.** APFS stores metadata alongside actual file data rather than in a fixed location. It uses B-trees containing file-system records and extent references.

### Key Implications
- There is no single file to read that contains all file metadata
- On HDDs, APFS enumeration is 3-20x slower than HFS+ because metadata is scattered
- On SSDs (which all modern Macs use), the random I/O penalty is minimal
- The `searchfs` syscall (which searched HFS+ catalog directly) is 5-6x slower on APFS

### References
- [APFS MFT equivalent discussion](https://github.com/libyal/libfsapfs/issues/21)
- [APFS enumeration performance analysis](https://bombich.com/blog/2019/09/12/analysis-apfs-enumeration-performance-on-rotational-hard-drives)
- [Apple File System Reference](https://developer.apple.com/support/downloads/Apple-File-System-Reference.pdf)

---

## 3. macOS Fast Scanning APIs

### 3.1 `getattrlistbulk` (Recommended Primary Approach)

The most promising API for fast directory scanning on macOS. It retrieves multiple directory entries with their attributes in a single system call.

**Key characteristics:**
- Extension of the deprecated `getattrlist()`
- Returns entries in bulk into a user-provided buffer (128KB recommended)
- Can return results without creating a vnode in-kernel, saving I/O
- Works on all filesystems (kernel handles non-native FS transparently)
- Required attributes: `ATTR_CMN_NAME` and `ATTR_CMN_RETURNED_ATTRS`
- Queryable: name, object type, inode, file size, allocated size, dates
- Call repeatedly until 0 entries returned

**Performance evidence:**
The `dumac` program (using `getattrlistbulk`) scanned 409,500 files in 4,095 directories in **521ms** vs BSD `du` at 3.33 seconds (6.4x faster). The traditional approach requires 400k+ individual `lstat` syscalls; `getattrlistbulk` batches these into far fewer calls.

**Usage pattern:**
```c
int fd = open(dirpath, O_RDONLY);
struct attrlist attrList = { .bitmapcount = ATTR_BIT_MAP_COUNT };
attrList.commonattr = ATTR_CMN_NAME | ATTR_CMN_OBJTYPE | ATTR_CMN_RETURNED_ATTRS;
attrList.fileattr = ATTR_FILE_TOTALSIZE | ATTR_FILE_ALLOCSIZE;
char buffer[128 * 1024];
while (getattrlistbulk(fd, &attrList, buffer, sizeof(buffer), 0) > 0) {
    // iterate through entries in buffer
}
```

### 3.2 `fts_open` / `fts_read` (BSD Tree Traversal)

Specialized BSD functions for recursive directory tree traversal.

- Fastest method on local HFS+ and APFS volumes in some benchmarks
- Handles the recursion internally
- Less control over per-directory batching than `getattrlistbulk`

### 3.3 `searchfs` (Volume-Wide Search)

Performs a flat search over all directory entries on a volume.

- Was very fast on HFS+ (searched catalog directly)
- **5-6x slower on APFS** - not recommended for new development
- Best for finding specific files by name, not for full enumeration

### 3.4 POSIX `readdir` / `opendir`

- Simpler API, cross-platform
- Significantly faster on NTFS and SMB mounts
- Requires separate `lstat` calls for file sizes (major overhead)

### 3.5 `NSFileManager.enumeratorAtURL`

- High-level Cocoa API with good performance on local disks
- Supports prefetching attributes to reduce syscalls
- Convenient but slightly more overhead than C APIs

### 3.6 FSEvents

- Monitors filesystem changes in real-time
- Useful for **incremental rescans** after initial scan completes
- Not suitable for initial full scan

### Performance Comparison Summary (External References)

| Method | 409K files benchmark | Best for |
|--------|---------------------|----------|
| `getattrlistbulk` | **521ms** | Bulk metadata retrieval |
| `fts_read` | ~800ms-1.5s | Recursive traversal |
| BSD `du` | 3.33s | Reference baseline |
| `readdir` + `lstat` | 3-5s | Simple cross-platform |
| `searchfs` (APFS) | Slow | Not recommended |

### Our Benchmark Results (MacDirStat, March 2026)

Tested on Apple Silicon Mac, APFS SSD, 49,579 files in 1,220 directories (15GB total).
Benchmark harness: Criterion. All scanners return identical results.

| Method | Time | Relative |
|--------|------|----------|
| `getattrlistbulk` + rayon | **44ms** | **1.0x (fastest)** |
| `getattrlistbulk` (sequential) | 65ms | 1.5x |
| `jwalk` (parallel, rayon) | 99ms | 2.3x |
| `std::fs::read_dir` (recursive) | 100ms | 2.3x |
| `walkdir` (sequential) | 107ms | 2.4x |
| `ignore` crate (parallel) | 126ms | 2.9x |

**Key takeaways:**
- `getattrlistbulk` + rayon is 2.3x faster than the best crate-based approach (`jwalk`)
- Adding rayon parallelism to `getattrlistbulk` gives ~33% improvement over sequential
- `jwalk` and `std::fs::read_dir` are nearly identical -- on cached APFS, parallelism helps less than expected since the bottleneck is kernel syscall overhead
- `ignore` is slowest despite parallelism, due to overhead from its filtering machinery

### Real-World Directory Benchmarks (warm cache, single pass)

Tested with `cargo run --release --bin investigate -- real-world`:

| Directory | Files | `getattrlistbulk_par` | `jwalk` | `std_readdir` | Speedup |
|-----------|------:|-----:|------:|------:|------:|
| `/usr` | 29K | **59ms** | 145ms | 486ms | 8.2x |
| `/Applications` | 244K | **278ms** | 2,091ms | 4,905ms | 17.6x |
| `~/Library` | 476K | **2,469ms** | 12,510ms | 15,668ms | 6.3x |
| `/System` | 4.5M | **37,116ms** | 126,136ms | 116,787ms | 3.1x |
| `data/bench_tree` | 50K | **50ms** | 99ms | 862ms | 17.1x |

**Key takeaways from real-world benchmarks:**
- `getattrlistbulk_par` dominates at all scales, with largest advantage on `/Applications` (17.6x)
- For `~/Library` (476K files), scan completes in 2.5 seconds -- acceptable for interactive use
- `/System` (4.5M files) takes 37s even with the fastest method -- progress UI is essential
- At larger scales, `jwalk` falls behind `std_readdir` (the overhead of its channel/rayon coordination exceeds the parallelism benefit when filesystem is the bottleneck)

### References
- [Performance considerations when reading directories on macOS](http://blog.tempel.org/2019/04/dir-read-performance.html)
- [Maybe the Fastest Disk Usage Program on macOS](https://healeycodes.com/maybe-the-fastest-disk-usage-program-on-macos)
- [Listing Files on macOS](https://jonnyzzz.com/blog/2020/08/12/listing-files/)
- [wtfs: fast bulk stat() with getattrlistbulk on macOS](https://ziggit.dev/t/wtfs-fast-bulk-stat-with-the-getattrlistbulk-syscall-on-macos/12109)
- [getattrlistbulk man page](https://www.manpagez.com/man/2/getattrlistbulk/)

---

## 4. Rust Crates for Directory Traversal

### 4.1 `jwalk` (Recommended)

**Parallel recursive directory walk with sorted, streamed results.**

- GitHub: [Byron/jwalk](https://github.com/Byron/jwalk)
- Uses Rayon's work-stealing thread pool for parallelism
- ~4x faster than `walkdir` for sorted results with metadata
- Parallelism at the directory level (helps with deep trees)

**Benchmarks** (walking Linux source code, from jwalk README):
| Crate | Unsorted | Sorted + Metadata |
|-------|----------|-------------------|
| `jwalk` | 60ms | 101ms |
| `ignore` | 74ms | 134ms |
| `walkdir` | 162ms | 423ms |

**Our benchmarks** (49K files, 15GB, APFS SSD, unsorted with metadata):
| Crate | Time |
|-------|------|
| `jwalk` | 99ms |
| `walkdir` | 107ms |
| `ignore` | 126ms |

Note: On cached APFS SSD, jwalk's parallelism advantage over walkdir is smaller (~8%) than on Linux ext4 because APFS metadata lookups are already fast with warm caches.

### 4.2 `walkdir`

**Sequential recursive directory walk.**

- GitHub: [BurntSushi/walkdir](https://github.com/BurntSushi/walkdir)
- Mature, well-tested, widely used
- Cross-platform
- No parallelism (single-threaded)

### 4.3 `ignore`

**Parallel walk with gitignore support.**

- Part of the ripgrep ecosystem
- Good parallelism but designed for filtering, not enumeration
- Higher latency for sorted results vs `jwalk`

### 4.4 Custom `getattrlistbulk` via FFI (Recommended for Maximum Performance)

No existing Rust crate wraps `getattrlistbulk`. The recommended approach:
1. Use `libc` crate for basic types
2. Define FFI bindings to `getattrlistbulk` manually (it's a single syscall)
3. Implement a safe Rust wrapper that iterates buffer entries
4. Combine with Rayon for parallel directory processing

This mirrors the `dumac` approach that achieved 521ms for 409K files:
- Open directory with `open(O_RDONLY)`
- Call `getattrlistbulk` in a loop with a 128KB buffer
- Parse variable-length entries from the buffer
- Use Rayon work-stealing pool for parallel subdirectory processing
- Shard inode sets for hardlink deduplication (128 shards, shift inode >> 8)

**Key optimization insight:** In profiling, ~91% of time is spent in kernel syscalls, meaning the Rust userspace code is already near-optimal once using `getattrlistbulk`.

---

## 5. How Existing macOS Disk Analyzers Work

### DaisyDisk
- Uses sunburst (concentric ring) visualization
- Scans drives up to 20x faster in v4 (uses optimized scanning)
- Supports parallel multi-disk scanning
- Commercial, Objective-C/Swift

### GrandPerspective
- Uses **treemap** visualization (same approach as windirstat)
- Open source (Objective-C)
- Incremental rescanning (only re-scans changed folders)
- Uses `NSFileManager` APIs

### OmniDiskSweeper
- Simple sorted list view (no treemap)
- Basic POSIX traversal

---

## 6. Recommended Scanning Architecture for MacDirStat

### Primary Strategy: Hybrid `getattrlistbulk` + `jwalk`

1. **Custom `getattrlistbulk` scanner** for macOS local volumes
   - FFI bindings to the syscall
   - 128KB buffer per directory
   - Rayon thread pool for parallel subdirectory processing
   - Hardlink deduplication via sharded inode HashSet

2. **`jwalk` fallback** for network volumes and non-APFS filesystems
   - Already handles cross-platform traversal well
   - Built-in parallelism via Rayon

3. **FSEvents watcher** for incremental updates after initial scan
   - Register for volume-level changes
   - Only rescan modified subtrees

### Threading Model
- Main scanning thread spawns Rayon tasks per directory
- Results streamed via `crossbeam-channel` to UI thread
- Atomic counters for progress reporting
- Tree built incrementally as results arrive

### Expected Performance (Validated with Real-World Data)
- 50K files (bench_tree): **50ms** with `getattrlistbulk_par`
- 476K files (`~/Library`): **2.5 seconds**
- 4.5M files (`/System`): **37 seconds** -- progress reporting essential
- Confirmed faster than all crate-based approaches at every scale

### Memory Usage (Validated)
Tested with `cargo run --release --bin investigate -- memory` on 50K files:

| Tree Representation | sizeof(Node) | RSS/node | Build time |
|---|---|---|---|
| `TreeNode` (String + Vec) | 56 bytes | 112 bytes | 895ms |
| `CompactNode` (Box<str> + Box<[T]>) | 40 bytes | 78 bytes | 128ms |

Extrapolation: 1M files = ~112MB (Vec) or ~78MB (compact). Both manageable.
**Recommendation:** Use `CompactNode` style -- 30% less memory, 7x faster build.

### Permission Handling (Validated)
Tested with `cargo run --release --bin investigate -- permissions`:

| Path | Permission errors | Impact |
|------|-----:|---|
| `/System` | 353 | All scanners get nearly identical counts despite errors (~0.001% variance) |
| `/private/var` | 192 | Same -- scanners silently skip inaccessible dirs |
| `~/Library` | 0 | Full access with user permissions |
| `/Library` | 14 | Minor -- mostly restricted preferences |

All scanners handle permission errors gracefully by silently skipping. No special error handling needed beyond counting errors for the UI.

### Hardlink, Firmlink, and Symlink Handling (Validated)
Tested with `cargo run --release --bin investigate -- hardlinks`:

**Hardlinks:** All scanners count each hardlink as a separate file (expected -- they are separate directory entries). Total size is triple-counted. **Must implement inode-based deduplication** for accurate size reporting. Add `ATTR_CMN_FILEID` to `getattrlistbulk` attributes and track seen inodes.

**Firmlinks:** `/Applications` is a true firmlink (identical counts from both `/Applications` and `/System/Volumes/Data/Applications`). `/usr` is NOT a simple firmlink -- `/usr` (29K files) includes read-only content from the sealed system volume that isn't in `/System/Volumes/Data/usr` (11K files). **Must be aware of volume boundaries** when scanning to avoid double-counting.

**Symlinks:** No scanner follows symlinks by default -- all report 2 files, not 3, when a symlink is present. This is correct behavior for a disk usage tool.

### Implementation Notes (from benchmarking)
- Use `libc::attrlist` and `libc::getattrlistbulk` directly (not custom FFI extern block)
- Correct constants: `ATTR_CMN_RETURNED_ATTRS = 0x80000000`, `ATTR_CMN_NAME = 0x00000001`, `ATTR_CMN_OBJTYPE = 0x00000008`, `ATTR_FILE_TOTALSIZE = 0x00000002`
- Open directories with `O_RDONLY` only (not `O_DIRECTORY`)
- Buffer parsing: entry starts with u32 length, then returned_attrs (5 x u32), then attrreference_t for name (i32 offset + u32 length), then obj_type (u32), then file_size (u64, only for files)
- Working implementation: `src/scan/getattrlistbulk.rs`
