//! Packing transparency into a normal video frame.
//!
//! H.264 has no transparency channel. So each frame we send is twice as wide:
//! the left half carries the colours, the right half carries the alpha as a grey
//! image (white = solid, black = see-through). The receiver splits them back apart.

/// RGBA w×h → RGBA (2w)×h. Left: colour (made opaque). Right: alpha as grey.
/// Fully transparent pixels get black colour: their colour is invisible anyway,
/// and black compresses better than leftover junk.
pub fn pack(rgba: &[u8], w: usize, h: usize) -> Vec<u8> {
    let mut out = vec![0u8; w * 2 * h * 4];
    for y in 0..h {
        let src = &rgba[y * w * 4..(y + 1) * w * 4];
        let dst = &mut out[y * w * 8..(y + 1) * w * 8];
        // Split the output row into its left (colour) and right (alpha) halves.
        let (left, right) = dst.split_at_mut(w * 4);
        for x in 0..w {
            let [r, g, b, a] = [src[x * 4], src[x * 4 + 1], src[x * 4 + 2], src[x * 4 + 3]];
            let colour = if a == 0 { [0, 0, 0, 255] } else { [r, g, b, 255] };
            left[x * 4..x * 4 + 4].copy_from_slice(&colour);
            right[x * 4..x * 4 + 4].copy_from_slice(&[a, a, a, 255]);
        }
    }
    out
}

/// The reverse of `pack`: colour from the left half, alpha from the right half's green.
/// `w` and `h` are the size of the *unpacked* image (half the packed width).
/// `stride` = bytes per row in the packed frame (may include padding).
pub fn unpack(packed: &[u8], stride: usize, w: usize, h: usize) -> Vec<u8> {
    let mut out = vec![0u8; w * h * 4];
    for y in 0..h {
        let row = &packed[y * stride..];
        for x in 0..w {
            let c = &row[x * 4..x * 4 + 3];
            let a = row[(w + x) * 4 + 1];
            out[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&[c[0], c[1], c[2], a]);
        }
    }
    out
}

// Tests: `cargo test -p vtc-core` runs these. They check pack → unpack gives back
// the original image (without real video compression in between).
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_then_unpack_round_trips() {
        let (w, h) = (3, 2);
        let rgba: Vec<u8> = vec![
            10, 20, 30, 255, /**/ 40, 50, 60, 128, /**/ 70, 80, 90, 0, //
            1, 2, 3, 4, /**/ 5, 6, 7, 8, /**/ 9, 10, 11, 12,
        ];
        let packed = pack(&rgba, w, h);
        let back = unpack(&packed, w * 2 * 4, w, h);
        // Fully transparent pixel (alpha 0) comes back with black colour, by design.
        let mut expected = rgba.clone();
        expected[8..11].copy_from_slice(&[0, 0, 0]);
        assert_eq!(back, expected);
    }
}
