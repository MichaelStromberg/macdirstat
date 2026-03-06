use egui::Color32;

pub const PALETTE_BRIGHTNESS: f64 = 0.6;

// 18 distinct hues, all readable on light backgrounds.
// Medium-dark, moderately saturated — good for tree text and cushion treemap base.
const RAW_PALETTE: [(u8, u8, u8); 18] = [
    (0, 0, 200),    // Blue
    (200, 0, 0),    // Red
    (0, 130, 0),    // Green
    (160, 0, 160),  // Purple
    (180, 100, 0),  // Brown/Orange
    (0, 130, 130),  // Teal
    (180, 0, 80),   // Crimson
    (80, 80, 180),  // Slate Blue
    (0, 100, 60),   // Forest Green
    (150, 60, 0),   // Rust
    (100, 0, 180),  // Violet
    (0, 110, 170),  // Cerulean
    (160, 60, 100), // Mauve
    (100, 120, 0),  // Olive
    (170, 0, 130),  // Magenta
    (0, 90, 120),   // Dark Cyan
    (120, 70, 40),  // Sienna
    (80, 0, 120),   // Indigo
];

/// Raw palette as Color32 — used for tree text where readability matters.
const PALETTE_TEXT: [Color32; 18] = {
    let mut result = [Color32::BLACK; 18];
    let mut i = 0;
    while i < 18 {
        let (r, g, b) = RAW_PALETTE[i];
        result[i] = Color32::from_rgb(r, g, b);
        i += 1;
    }
    result
};

/// Normalized palette (brightness 0.6) — used as base for cushion treemap shading.
const PALETTE_TREEMAP: [Color32; 18] = {
    let mut result = [Color32::BLACK; 18];
    let mut i = 0;
    while i < 18 {
        let (r, g, b) = RAW_PALETTE[i];
        let (nr, ng, nb) = normalize_color(r, g, b, PALETTE_BRIGHTNESS);
        result[i] = Color32::from_rgb(nr, ng, nb);
        i += 1;
    }
    result
};

/// WinDirStat MakeBrightColor + NormalizeColor algorithm.
/// Scales RGB so average brightness = target, then redistributes overflow.
const fn normalize_color(r: u8, g: u8, b: u8, target: f64) -> (u8, u8, u8) {
    let sum = r as f64 + g as f64 + b as f64;
    if sum < 0.001 {
        let v = (target * 255.0) as u8;
        return (v, v, v);
    }

    let f = 3.0 * target * 255.0 / sum;
    let mut rf = r as f64 * f;
    let mut gf = g as f64 * f;
    let mut bf = b as f64 * f;

    // NormalizeColor: redistribute overflow
    let mut iterations = 0;
    while iterations < 10 {
        let mut overflow = 0.0;
        let mut under_count = 0;

        if rf > 255.0 {
            overflow += rf - 255.0;
            rf = 255.0;
        } else {
            under_count += 1;
        }
        if gf > 255.0 {
            overflow += gf - 255.0;
            gf = 255.0;
        } else {
            under_count += 1;
        }
        if bf > 255.0 {
            overflow += bf - 255.0;
            bf = 255.0;
        } else {
            under_count += 1;
        }

        if overflow < 0.5 || under_count == 0 {
            break;
        }

        let add = overflow / under_count as f64;
        if rf < 255.0 {
            rf += add;
        }
        if gf < 255.0 {
            gf += add;
        }
        if bf < 255.0 {
            bf += add;
        }

        iterations += 1;
    }

    let cr = if rf < 0.0 {
        0
    } else if rf > 255.0 {
        255
    } else {
        rf as u8
    };
    let cg = if gf < 0.0 {
        0
    } else if gf > 255.0 {
        255
    } else {
        gf as u8
    };
    let cb = if bf < 0.0 {
        0
    } else if bf > 255.0 {
        255
    } else {
        bf as u8
    };

    (cr, cg, cb)
}

const DIR_COLOR: Color32 = Color32::from_rgb(100, 100, 100);

/// Maps extensions to colors. Stores both text-readable and treemap-normalized palettes.
pub struct ColorMap {
    map_text: std::collections::HashMap<Box<str>, Color32>,
    map_treemap: std::collections::HashMap<Box<str>, Color32>,
}

impl ColorMap {
    /// Build a color map from sorted extensions (largest first).
    pub fn from_extensions(extensions: &[(Box<str>, u64)]) -> Self {
        let mut map_text = std::collections::HashMap::new();
        let mut map_treemap = std::collections::HashMap::new();
        for (i, (ext, _)) in extensions.iter().enumerate() {
            let idx = i % PALETTE_TEXT.len();
            map_text.insert(ext.clone(), PALETTE_TEXT[idx]);
            map_treemap.insert(ext.clone(), PALETTE_TREEMAP[idx]);
        }
        Self {
            map_text,
            map_treemap,
        }
    }

    /// Get readable text color for an extension (raw palette, good contrast on light backgrounds).
    pub fn get(&self, extension: &str) -> Color32 {
        if extension.is_empty() {
            return DIR_COLOR;
        }
        self.map_text
            .get(extension)
            .copied()
            .unwrap_or(PALETTE_TEXT[PALETTE_TEXT.len() - 1])
    }

    /// Get treemap base color for an extension (normalized to brightness 0.6 for cushion shading).
    pub fn get_treemap(&self, extension: &str) -> Color32 {
        if extension.is_empty() {
            return DIR_COLOR;
        }
        self.map_treemap
            .get(extension)
            .copied()
            .unwrap_or(PALETTE_TREEMAP[PALETTE_TREEMAP.len() - 1])
    }

    pub fn dir_color() -> Color32 {
        DIR_COLOR
    }
}
