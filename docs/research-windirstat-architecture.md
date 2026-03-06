# WinDirStat Architecture Analysis

Analysis of the windirstat codebase at `legacy/windirstat/` to inform the MacDirStat port.

## 1. Overall Architecture

WinDirStat is an MFC (Microsoft Foundation Classes) application (~26,000 lines C++) following an MVC pattern:

```
windirstat/
├── Item.h/cpp                  # Core data model (directory tree nodes)
├── DirStatDoc.h/cpp            # Document/model layer (MVC)
├── MainFrame.h/cpp             # Main window frame
├── Finder.h                    # Abstract scanner interface
├── FinderBasic.h/cpp           # Win32 API scanner
├── FinderNtfs.h/cpp            # NTFS MFT scanner
├── Controls/
│   ├── TreeMap.h/cpp           # Treemap layout + rendering engine
│   └── ExtensionListControl.h  # Extension statistics list
├── Views/
│   ├── TreeMapView.h/cpp       # Treemap display view
│   ├── FileTreeView.h/cpp      # Directory tree list
│   ├── ExtensionView.h/cpp     # File type statistics
│   ├── FileTopView.h/cpp       # Top N largest files
│   ├── FileDupeView.h/cpp      # Duplicate file detection
│   ├── FileSearchView.h/cpp    # Search results
│   └── FileTabbedView.h/cpp    # Tabbed container
└── Dialogs/                    # Configuration dialogs
```

## 2. Data Model: CItem

`CItem` (Item.h/cpp) is the central data structure representing files and directories.

### Type System
Bitmask-based types: `IT_DRIVE`, `IT_DIRECTORY`, `IT_FILE`, `IT_FREESPACE`, `IT_UNKNOWN`, `IT_HLINKS`

### Key Fields
- Name (inline string storage for memory efficiency)
- Size (logical and physical)
- File/folder counts (atomic for thread safety)
- Children vector (lazy-allocated `CHILDINFO` for leaf nodes)
- Rectangle cache (for treemap hit testing)
- Color (from extension mapping)

### Interfaces Implemented
- `CTreeListItem` - for tree/list display
- `CTreeMap::Item` - for treemap visualization (provides `TmiGetSize()`, `TmiGetGraphColor()`, etc.)

## 3. Scanning Architecture

### Scanner Interface (`Finder.h`)
Abstract interface allowing pluggable scanning strategies:
- `FinderBasic`: Standard Win32 API using `NtQueryDirectoryFile` with 4MB buffer
- `FinderNtfs`: Direct NTFS MFT parsing for ~46x speedup on NTFS volumes

### Scanning Flow
1. `CDirStatDoc::StartScanningEngine()` - spawns worker thread (`std::jthread`)
2. `CItem::ScanItems()` - main loop, selects Finder based on `COptions::UseFastScanEngine`
3. Items pushed to `BlockingQueue<CItem*>` for parallel processing
4. `CItem::ScanItemsFinalize()` - post-processing: sorting children by size (descending), computing aggregates

### Threading Model
- `std::jthread` for scanning thread
- `BlockingQueue<CItem*>` for work distribution
- `std::atomic` with `relaxed` ordering for extension statistics
- `std::mutex` for extension data map access
- UI updates marshaled through Windows message thread

### Special Cases
- Sparse/compressed files (separate size APIs)
- Reparse points (symlinks, junctions, mount points, cloud storage)
- Hard links (tracked separately with `IT_HLINKS`)
- Long paths (>260 chars via `\\?\` prefix)
- Free space and unknown space items

## 4. Treemap Engine (`Controls/TreeMap.h/cpp`)

### CTreeMap::Item Interface
```cpp
virtual bool TmiIsLeaf() const = 0;
virtual CRect TmiGetRectangle() const = 0;
virtual void TmiSetRectangle(const CRect& rc) = 0;
virtual COLORREF TmiGetGraphColor() const = 0;
virtual int TmiGetChildCount() const = 0;
virtual Item* TmiGetChild(int c) const = 0;
virtual ULONGLONG TmiGetSize() const = 0;
```

### Layout Algorithms
Two styles available (see `research-visualization.md` for details):
- **KDirStatStyle**: Row-based with 0.4 minimum proportion constraint
- **SequoiaViewStyle**: Classical squarification (greedy worst-case ratio minimization)

### Rendering Pipeline
1. Allocate COLORREF pixel buffer (width * height)
2. Stack-based iterative traversal (avoids recursion stack overflow)
3. For each node: `AddRidge()` to accumulate surface coefficients
4. For each leaf: `RenderLeaf()` -> `RenderRectangle()` -> `DrawCushion()` or `DrawSolidRect()`
5. Create Windows bitmap from pixel buffer
6. BitBlt to screen DC

### Cushion Shading
Surface is 4 coefficients `[a_x, a_y, b_x, b_y]` defining `z(x,y) = a_x*x^2 + a_y*y^2 + b_x*x + b_y*y`.
Per-pixel Lambertian shading with configurable light source. See `research-visualization.md` Section 3.2 for full algorithm.

## 5. Extension Statistics

### Data Structure
```cpp
struct SExtensionRecord {
    std::atomic<ULONGLONG> files;  // File count
    std::atomic<ULONGLONG> bytes;  // Total size
    COLORREF color;                // Display color
};
using CExtensionData = std::unordered_map<std::wstring, SExtensionRecord>;
```

### Collection
- `CItem::GetExtension()` - extracts lowercase extension
- `CItem::ExtensionDataAdd()` - increments atomically during scan
- Thread-safe via `std::mutex m_extensionMutex`
- Stack-based traversal in `ExtensionDataProcessChildren()`

### Display
`CExtensionListControl` - 6 columns:
- Extension, Color preview (cushion-rendered swatch), Description, Bytes, Bytes%, Files

## 6. UI Layout

### Main Frame Structure
```
┌─────────────────────────────────────────────┐
│ Menu Bar | Toolbar                          │
├───────────────────────┬─────────────────────┤
│                       │                     │
│  File Tree View       │  Extension View     │
│  (with tabs:          │  (color, ext,       │
│   All Files,          │   desc, bytes,      │
│   Duplicates,         │   %, files)         │
│   Search)             │                     │
│                       │                     │
├───────────────────────┴─────────────────────┤
│                                             │
│  Treemap View                               │
│  (cushion-shaded, full width)               │
│                                             │
├─────────────────────────────────────────────┤
│ Status Bar (pacman progress, memory usage)  │
└─────────────────────────────────────────────┘
```

### Splitter Windows
`CMySplitterWnd` maintains user-adjusted split ratios with persistence.

### Selection Management
- Logical focus tracking across views (tree, extension, top files)
- Clicking treemap selects corresponding tree node
- Clicking tree highlights in treemap
- Extension selection highlights all files of that type

### Interaction Features
- Treemap hover: tooltip with file path and size
- Treemap click: navigate to item in tree
- Treemap zoom: double-click to zoom into subtree
- Tree selection: highlights item in treemap with dotted border
- Context menu: open, delete, properties, explorer

## 7. Key Porting Considerations

### What Translates Directly
- Treemap layout algorithms (pure math, no platform deps)
- Cushion shading algorithm (pure math)
- Color system and palette
- Extension statistics collection
- Data model (CItem equivalent in Rust)

### What Needs Platform Replacement
- MFC -> egui/Slint (or other Rust GUI)
- Win32 file scanning -> `getattrlistbulk` + `jwalk` (implemented and benchmarked in `src/scan/`)
- GDI bitmap rendering -> wgpu or egui Painter
- Windows shell integration -> macOS Launch Services
- Registry settings -> plist or config file

### What Can Be Improved
- egui `Painter` for treemap rendering (benchmarked: 500K rects in 8ms, no GPU shader needed)
- Parallel scanning from the start (windirstat added NTFS scanner later)
  - Already implemented: `getattrlistbulk` + rayon scans 50K files in 50ms, 476K in 2.5s, 4.5M in 37s
  - 2.3-17.6x faster than crate-based approaches depending on directory structure
- Compact tree nodes (Box<str> + Box<[T]>: 40 bytes/node, ~78 bytes RSS/node at 50K scale)
- Incremental updates via FSEvents
- Hardlink deduplication via inode tracking (all scanners currently triple-count)
- Firmlink-aware scanning to avoid double-counting `/usr` and `/System/Volumes/Data/usr`
