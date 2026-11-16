//! Packing transparency into a normal video frame.
//!
//! H.264 has no transparency channel. So each frame we send is twice as wide:
//! the left half carries the colours, the right half carries the alpha as a grey
//! image (white = solid, black = see-through). The receiver splits them back apart.
//!
//! Speed note: each RGBA pixel is 4 bytes, so we handle it as one 32-bit number
//! (`u32`). In memory the bytes are R, G, B, A; read as a little-endian u32 that is
//! `R | G<<8 | B<<16 | A<<24`. Working on whole numbers with no `if`s lets the
//! compiler process many pixels per CPU instruction ("vectorising").

const OPAQUE: u32 = 0xFF00_0000; // alpha = 255, colour = 0

/// One pixel: colour (made opaque) and alpha-as-grey.
#[inline(always)]
fn pack_pixel(px: u32) -> (u32, u32) {
    let a = px >> 24;
    // Fully transparent → black colour (invisible anyway, and compresses better).
    // `mask` is all ones when a > 0, all zeros when a == 0 — no `if` needed.
    let mask = ((a != 0) as u32).wrapping_neg() & 0x00FF_FFFF;
    let colour = (px & mask) | OPAQUE;
    let grey = a * 0x0001_0101 | OPAQUE; // a copied into R, G and B
    (colour, grey)
}

/// RGBA w×h → RGBA (2w)×h. Left: colour (made opaque). Right: alpha as grey.
pub fn pack(rgba: &[u8], w: usize, h: usize) -> Vec<u8> {
    let mut out = vec![0u8; w * 2 * h * 4];
    pack_into(rgba, w, h, &mut out);
    out
}

/// Same as `pack`, but writes into an existing buffer of at least (2w)×h×4 bytes.
/// Saves allocating and zero-filling a new 16 MB buffer every frame.
pub fn pack_into(rgba: &[u8], w: usize, h: usize, out: &mut [u8]) {
    let src_rows = rgba.chunks_exact(w * 4).take(h);
    for (src, dst) in src_rows.zip(out.chunks_exact_mut(w * 8)) {
        // Split the output row into its left (colour) and right (alpha) halves.
        let (left, right) = dst.split_at_mut(w * 4);
        let pixels = src.chunks_exact(4).zip(left.chunks_exact_mut(4)).zip(right.chunks_exact_mut(4));
        for ((s, l), r) in pixels {
            let (colour, grey) = pack_pixel(u32::from_le_bytes([s[0], s[1], s[2], s[3]]));
            l.copy_from_slice(&colour.to_le_bytes());
            r.copy_from_slice(&grey.to_le_bytes());
        }
    }
}

/// The reverse of `pack`: colour from the left half, alpha from the right half's green.
/// `w` and `h` are the size of the *unpacked* image (half the packed width).
/// `stride` = bytes per row in the packed frame (may include padding).
pub fn unpack(packed: &[u8], stride: usize, w: usize, h: usize) -> Vec<u8> {
    let mut out = vec![0u8; w * h * 4];
    for (y, dst) in out.chunks_exact_mut(w * 4).enumerate() {
        let row = &packed[y * stride..y * stride + w * 8];
        let (left, right) = row.split_at(w * 4);
        let pixels = left.chunks_exact(4).zip(right.chunks_exact(4)).zip(dst.chunks_exact_mut(4));
        for ((c, a), d) in pixels {
            d.copy_from_slice(&[c[0], c[1], c[2], a[1]]);
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
        // Spot-check the packed layout: pixel 1 colour (opaque) and its alpha as grey.
        assert_eq!(&packed[4..8], &[40, 50, 60, 255]);
        assert_eq!(&packed[(3 + 1) * 4..(3 + 1) * 4 + 4], &[128, 128, 128, 255]);
        let back = unpack(&packed, w * 2 * 4, w, h);
        // Fully transparent pixel (alpha 0) comes back with black colour, by design.
        let mut expected = rgba.clone();
        expected[8..11].copy_from_slice(&[0, 0, 0]);
        assert_eq!(back, expected);
    }

    #[test]
    fn unpack_handles_row_padding() {
        let (w, h) = (2, 2);
        let rgba: Vec<u8> = (0..(w * h * 4) as u8).map(|v| v | 1).collect(); // no zero alpha
        let packed = pack(&rgba, w, h);
        // Re-lay the packed rows with 8 bytes of padding each, like a GPU might.
        let stride = w * 8 + 8;
        let mut padded = vec![0u8; stride * h];
        for y in 0..h {
            padded[y * stride..y * stride + w * 8].copy_from_slice(&packed[y * w * 8..(y + 1) * w * 8]);
        }
        assert_eq!(unpack(&padded, stride, w, h), rgba);
    }
}
