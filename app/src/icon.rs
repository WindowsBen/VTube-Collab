//! The app icon as pixels: a blue tile with two overlapping avatar "heads" (pink and
//! cyan). The same drawing as `xp::paint_app_icon`, for places that need an image:
//! the window icon, and the icon built into VTubeCollab.exe (build.rs uses this file
//! too, so Explorer, the taskbar and Task Manager all show the same icon).

/// The same icon as raw RGBA pixels, for the taskbar (`size`×`size`). Each pixel is
/// sampled 4×4 times and averaged, so edges come out smooth.
pub fn rgba(size: u32) -> Vec<u8> {
    let mut out = vec![0u8; (size * size * 4) as usize];
    let s = size as f32 / 16.0;
    let (pink, cyan) = ([0xFF, 0x8C, 0xCB], [0x7F, 0xE3, 0xFF]);
    for y in 0..size {
        for x in 0..size {
            let mut acc = [0.0f32; 4];
            for sy in 0..4 {
                for sx in 0..4 {
                    let px = (x as f32 + (sx as f32 + 0.5) / 4.0) / s;
                    let py = (y as f32 + (sy as f32 + 0.5) / 4.0) / s;
                    if let Some(c) = colour(px, py, pink, cyan) {
                        for i in 0..3 {
                            acc[i] += c[i] as f32;
                        }
                        acc[3] += 255.0;
                    }
                }
            }
            let i = ((y * size + x) * 4) as usize;
            let coverage = acc[3] / 255.0;
            if coverage > 0.0 {
                for c in 0..3 {
                    out[i + c] = (acc[c] / coverage) as u8;
                }
                out[i + 3] = (acc[3] / 16.0) as u8;
            }
        }
    }
    out
}

/// Colour of the 16×16 icon at (x, y), or None outside it.
fn colour(x: f32, y: f32, pink: [u8; 3], cyan: [u8; 3]) -> Option<[u8; 3]> {
    // Rounded tile (corner radius 3).
    let (cx, cy) = (x.clamp(3.0, 13.0), y.clamp(3.0, 13.0));
    if (x - cx).powi(2) + (y - cy).powi(2) > 9.0 {
        return None;
    }
    let head = |hx: f32, r: f32| (x - hx).powi(2) + (y - 8.5).powi(2) <= r * r;
    if head(10.4, 4.2) {
        return Some(if head(10.4, 3.4) { cyan } else { [255, 255, 255] });
    }
    if head(5.6, 4.2) {
        return Some(if head(5.6, 3.4) { pink } else { [255, 255, 255] });
    }
    let t = y / 16.0;
    Some([(0x6C as f32 + (0x1A - 0x6C) as f32 * t) as u8, (0xB2 as f32 + (0x4E - 0xB2) as f32 * t) as u8, (0xFF as f32 + (0xD6 - 0xFF) as f32 * t) as u8])
}
