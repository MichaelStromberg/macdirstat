# Visualization Research: GUI Frameworks, Treemaps, and Crates

## 1. Reference UI Analysis

### WinDirStat Layout
The WinDirStat interface consists of three main panels in a vertical split:

1. **Top-left: Directory Tree View** - A tree list with sortable columns:
   - Name, Subtree Percentage (with bar), Percentage, Physical Size, Logical Size, Files, Last Change
   - Shows folder hierarchy with expand/collapse
   - Blue percentage bars inline with the tree

2. **Top-right: Extension Statistics** - A list showing file types:
   - Color swatch, Extension name, Description
   - Shows what percentage of disk each file type consumes

3. **Bottom: Treemap Visualization** - Full-width cushion treemap:
   - Each file represented as a colored rectangle proportional to size
   - Colors correspond to file extension (matching the extension list)
   - Cushion shading gives 3D depth effect showing hierarchy
   - Selection highlight with dotted border
   - Hover tooltips showing file path and size

### WizTree Layout
WizTree has a similar but modernized layout:

1. **Top bar**: Drive selector, Scan button, disk space summary (Total/Used/Free)
2. **Top-left: Tree View** with tabs (Tree View / File View):
   - Columns: Folder, % of Parent, Size, Allocated, Items, Files, Folders, Modified, Attributes
   - Inline percentage bars
   - Row highlighting on selection
3. **Top-right: Extension list**:
   - Color swatch, Extension, File Type description, Percent
4. **Bottom: Treemap** with labels showing folder names and sizes directly on rectangles
   - Flat colored rectangles (no cushion shading)
   - Text labels on larger rectangles showing path and size
   - Brighter, more saturated colors than WinDirStat

---

## 2. Rust GUI Framework Evaluation

### Framework Comparison for MacDirStat

Based on the [2025 Survey of Rust GUI Libraries](https://www.boringcactus.com/2025/04/13/2025-survey-of-rust-gui-libraries.html) and additional research:

| Framework | Custom 2D Drawing | Tree View | macOS Support | Maturity |
|-----------|-------------------|-----------|---------------|----------|
| **egui** | Excellent (epaint) | Manual impl | Good (eframe) | High |
| **Slint** | Limited (DSL) | Built-in | Good | High |
| **iced** | Canvas widget | No built-in | Good | Medium |
| **Tauri** | HTML Canvas/WebGL | HTML-based | Good | High |
| **Dioxus** | HTML Canvas | HTML-based | Good | High |
| **Xilem** | Custom (Masonry) | Possible | Early | Low |
| **wgpu + winit** | Full control | Manual impl | Excellent | High |

### Detailed Assessment

#### egui (Recommended -- Validated by Benchmarks)
- **Rendering**: Immediate-mode with epaint backend. Anti-aliased lines, circles, text, convex polygons
- **Custom drawing**: Excellent - `Painter` API allows drawing arbitrary shapes per-frame
- **Scene container** (v0.31+): Pannable, zoomable canvas for complex visualizations
- **Tree views**: No built-in widget, but `CollapsingHeader` can approximate; community `egui_extras` has table support
- **macOS**: Runs via eframe on Metal/OpenGL
- **Strengths**: Simple API, productive, fast iteration, good screen reader support
- **Weaknesses**: Not native look and feel, IME issues, immediate-mode overhead for large UIs
- [GitHub: emilk/egui](https://github.com/emilk/egui)

**Paint Command Generation (Benchmarked, March 2026):**

| Rectangles | Paint generation time | Equivalent FPS |
|-----:|-----:|-----:|
| 1K | 18us | 55,000 |
| 10K | 157us | 6,400 |
| 50K | 1.6ms | 625 |
| 100K | 1.9ms | 526 |
| 500K | 8.2ms | 122 |

**Conclusion:** egui's Painter API can generate paint commands for 500K rectangles in 8.2ms. Including tessellation and GPU rendering overhead, 100K+ rectangles at 60fps is achievable. A custom wgpu shader is NOT needed for the treemap -- egui alone is sufficient. This removes significant implementation complexity.

#### Slint (Recommended for Production Native Feel)
- **Rendering**: DSL-driven with native-looking widgets
- **Custom drawing**: More limited - designed around declaring UI in `.slint` files
- **Tree views**: Has some built-in list/tree support
- **macOS**: Good native integration
- **Strengths**: Excellent developer tooling, accessibility, two-way bindings
- **Weaknesses**: DSL adds learning curve, custom canvas drawing less flexible
- [slint.dev](https://slint.dev/)

#### wgpu + winit (Recommended for Treemap Rendering)
- **Rendering**: Full GPU access via Metal (macOS), Vulkan, DX12, OpenGL
- **Custom drawing**: Complete control - write shaders for exactly what you need
- **Strengths**: Maximum performance for treemap rendering, GPU-accelerated
- **Weaknesses**: Low-level, need to build everything from scratch
- [wgpu.rs](https://wgpu.rs/)

#### Tauri / Dioxus
- **Web-based**: Use system WebView (WebKit on macOS)
- **Custom drawing**: HTML5 Canvas or WebGL
- **Tree views**: Trivial with HTML/CSS (virtual scrolling libraries available)
- **Strengths**: Mature ecosystem for UI components
- **Weaknesses**: IPC overhead, not truly native, memory overhead
- Performance benchmark shows startup and rendering lag vs native

#### iced
- **Canvas widget**: Supports custom 2D drawing
- **Tree views**: No built-in widget
- **macOS**: Works but accessibility poor (open issue for 4.5+ years)
- Not recommended due to accessibility gaps and missing widgets

### Recommendation: egui for Everything (Validated)

**Use egui for both the application shell AND treemap rendering.**

Benchmarks confirm egui's `Painter` API can handle 500K rectangles at 122 FPS equivalent (paint generation only). Combined with the `treemap` crate's 62ms nested layout for 500K items, the full rendering pipeline fits within a 16ms frame budget for typical disk scans (< 100K visible rectangles).

- egui handles the tree view, menus, toolbar, extension list, status bar
- egui `Painter::rect_filled()` renders treemap rectangles directly
- No custom wgpu shader needed -- removes major implementation complexity
- Cushion shading can be done via CPU-rendered texture uploaded as egui `TextureHandle`
- If cushion shading proves too slow in CPU, a wgpu compute shader remains an option

Previous recommendation was a hybrid egui + custom wgpu approach, but benchmarks show this is unnecessary.

---

## 3. Treemap Visualization

### 3.1 Squarified Treemap Algorithm

The squarified treemap algorithm recursively tessellates a rectangle into sub-rectangles with aspect ratios approaching 1.0 (squares).

**Existing Rust crate: `treemap`**
- [GitHub: bacongobbler/treemap-rs](https://github.com/bacongobbler/treemap-rs)
- Implements squarified treemap layout
- API: `TreemapLayout::layout_items(&mut items, bounds)`
- Items implement `Mappable` trait
- MIT licensed, 38 stars
- **Low maintenance** (last update ~2021, 22 commits)
- Provides layout only (no rendering) - good for our needs

**Layout Performance (Benchmarked, March 2026):**

| Items | Flat layout | Nested layout |
|------:|-----:|-----:|
| 1K | 65us | 13us |
| 10K | 1.3ms | 363us |
| 50K | 13ms | 2.7ms |
| 100K | 37ms | 7ms |
| 500K | 402ms | 62ms |

Nested layout (the real WinDirStat pattern: layout children within each parent's bounds) is **5-6x faster** than flat layout because each sub-layout operates on smaller arrays. At 500K files, nested layout completes in 62ms -- well within interactive budget. Relayout on window resize is feasible even for the largest datasets.

**WinDirStat implements two styles:**
1. **KDirStatStyle**: Row-based layout with minimum proportion constraint (0.4). Children laid out in rows alternating horizontal/vertical. Simpler but produces more elongated rectangles.
2. **SequoiaViewStyle**: Classical squarification per van Wijk. Greedily adds children to a row until aspect ratio worsens. Produces better-looking squares.

Both are in `Controls/TreeMap.cpp` (lines 581-701 for KDirStat, 273-408 for SequoiaView).

### 3.2 Cushion Treemap Rendering (3D Shading Effect)

The cushion effect is the signature visual of WinDirStat. It uses parabolic surface shading to encode hierarchy depth.

**Algorithm (from WinDirStat `TreeMap.cpp:727-791`):**

The surface is represented by 4 coefficients `[a_x, a_y, b_x, b_y]` defining a height function:
```
z(x, y) = a_x * x^2 + a_y * y^2 + b_x * x + b_y * y
```

**Ridge Addition** (line 775-791): Each time a rectangle is subdivided, a parabolic ridge is added:
```
For a rectangle with bounds [left, right, top, bottom]:
  h4 = 4 * h  (h is the height factor, scaled by scaleFactor^depth)

  wf = h4 / width
  surface[0] -= wf         // a_x coefficient
  surface[2] += wf * (right + left)  // b_x coefficient

  hf = h4 / height
  surface[1] -= hf         // a_y coefficient
  surface[3] += hf * (bottom + top)  // b_y coefficient
```

**Pixel Shading** (line 727-773): For each pixel (ix, iy):
```
// Surface normal from partial derivatives
nx = -(2 * a_x * (ix + 0.5) + b_x)
ny = -(2 * a_y * (iy + 0.5) + b_y)

// Lambertian shading: dot product of normal with light direction
cosa = (nx * lx + ny * ly + lz) / sqrt(nx^2 + ny^2 + 1.0)
cosa = min(cosa, 1.0)

// Final pixel brightness
pixel = max(Is * cosa, 0.0) + Ia  // Is = 1 - Ia (shading intensity)
pixel *= brightness / PALETTE_BRIGHTNESS

// Apply to base color
red = base_red * pixel
green = base_green * pixel
blue = base_blue * pixel
```

**Default Parameters:**
- `height` = 0.38 (initial ridge height, H)
- `scaleFactor` = 0.91 (height multiplier per depth level, F)
- `ambientLight` = 0.13 (minimum brightness, Ia)
- `lightSourceX` = -1.0 (light from left)
- `lightSourceY` = -1.0 (light from top)
- `brightness` = 0.88 (overall brightness)
- `PALETTE_BRIGHTNESS` = 0.6 (palette normalization target)

**Light vector normalization:**
```
lz = 10 (fixed)
len = sqrt(lx^2 + ly^2 + lz^2)
(m_lx, m_ly, m_lz) = (lx/len, ly/len, lz/len)
```

### 3.3 GPU-Accelerated Treemap Rendering

The cushion shading algorithm is embarrassingly parallel (each pixel independent). A wgpu compute or fragment shader could render it:

```wgsl
// Fragment shader for cushion treemap
struct CushionData {
    surface: vec4<f32>,  // [a_x, a_y, b_x, b_y]
    color: vec3<f32>,
    bounds: vec4<f32>,   // [left, top, right, bottom]
};

@fragment
fn cushion_fragment(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let nx = -(2.0 * surface.x * (pos.x + 0.5) + surface.z);
    let ny = -(2.0 * surface.y * (pos.y + 0.5) + surface.w);
    let cosa = dot(vec3(nx, ny, 1.0), light_dir) / length(vec3(nx, ny, 1.0));
    let brightness = max(Is * clamp(cosa, 0.0, 1.0), 0.0) + Ia;
    return vec4(color * brightness, 1.0);
}
```

For a treemap with 100K+ rectangles, GPU rendering would be dramatically faster than CPU per-pixel computation.

### 3.4 Color System

WinDirStat uses an 18-color palette, each normalized to brightness 0.6:
```
Blue, Red, Green, Yellow, Cyan, Magenta, Orange, Dodger Blue,
Hot Pink, Lime Green, Violet, Spring Green, Deep Pink, Sky Blue,
Orange Red, Aquamarine, Indigo, White
```

Colors are assigned to file extensions based on extension sort order. The `MakeBrightColor` function normalizes any color to a target brightness while preserving hue ratios. Special flags darken free space (0.66x) and brighten unknown items (1.2x).

---

## 4. Tree View / List View Components

### Requirements
- Hierarchical tree with expand/collapse
- Multiple sortable columns (Name, Size, %, Files, Modified)
- Inline percentage bars
- Virtual scrolling (100K+ nodes)
- Selection sync with treemap

### egui Approach
- `egui_extras::TableBuilder` for sortable column tables
- Custom tree rendering using `CollapsingHeader` or manual indent
- Virtual scrolling via `ScrollArea` with `show_rows()` (only renders visible rows)
- Can draw inline progress bars using `Painter`

### Slint Approach
- Built-in `ListView` with model-based data
- Supports custom item delegates
- Better out-of-box tree support

### Key Crates
- **`egui_extras`**: Table widget with sortable columns, row selection
- **`egui_dock`**: Docking/tabbed layout (for File tree / Duplicate / Search tabs)

---

## 5. Existing Rust Disk Analyzers (Reference Implementations)

### dust
- [GitHub: bootandy/dust](https://github.com/bootandy/dust)
- CLI tool, "more intuitive `du`"
- ASCII tree + bar visualization
- Uses `walkdir` for traversal

### dua-cli
- [GitHub: Byron/dua-cli](https://github.com/Byron/dua-cli)
- CLI + TUI (terminal UI)
- Uses `jwalk` for parallel traversal (same author)
- Interactive navigation and deletion
- Good reference for scanning architecture

### diskonaut
- Terminal-based treemap visualization
- Uses ncurses-style rendering
- Scans directory then maps to memory
- macOS and Linux support

### durs
- [GitHub: rust-rs/durs](https://github.com/rust-rs/durs)
- Fast disk usage analyzer with visualizations

---

## 6. Recommended Visualization Stack

### Core Rendering
| Component | Crate/Approach |
|-----------|---------------|
| Window + event loop | `eframe` (wraps winit + wgpu) |
| Application UI | `egui` |
| Treemap layout | `treemap` crate or custom impl from windirstat |
| Treemap rendering | egui `Painter::rect_filled()` -- **benchmarked: 500K rects in 8ms** |
| Tree view | `egui_extras::Table` + custom tree logic |
| Tab panel | `egui_dock` |
| Charts | `egui_plot` for extension statistics |

### Implementation Priority
1. **Phase 1**: egui application shell with tree view and basic flat treemap
   - Scanning backend already implemented and benchmarked (see `research-disk-scanning.md`)
   - `getattrlistbulk` + rayon: 50K files in 50ms, 476K files in 2.5s
   - Treemap layout: 500K nested in 62ms, egui paint: 500K rects in 8ms
2. **Phase 2**: Cushion shading (CPU-rendered texture uploaded to egui)
3. **Phase 3**: Extension statistics panel, interactive features
4. **Phase 4**: Hardlink dedup (inode tracking), firmlink-aware volume boundaries

### Key Crate Versions (as of 2025-2026)
- `egui` / `eframe`: 0.31+ (Scene container support)
- `egui_extras`: matching egui version
- `egui_dock`: 0.14+
- `treemap`: 0.3+
- `wgpu`: 24.0+
- `winit`: 0.30+
