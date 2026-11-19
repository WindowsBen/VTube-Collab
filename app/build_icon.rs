//! Shared build step (used by app/build.rs and launcher/build.rs): builds the app icon
//! and the program's name into an .exe, so Explorer, the taskbar, pinned shortcuts and
//! Task Manager show them. The icon is drawn by app/src/icon.rs (`crate::icon`).

/// Build the icon and program details into the .exe being built, which will be
/// called `file_name`.
pub fn embed(file_name: &str) {
    println!("cargo:rerun-if-changed=../app/src/icon.rs");
    println!("cargo:rerun-if-changed=../app/build_icon.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let ico = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("VTubeCollab.ico");
    std::fs::write(&ico, ico_file()).expect("couldn't write the icon file");

    let mut res = winresource::WindowsResource::new();
    res.set_icon(ico.to_str().unwrap())
        .set("FileDescription", "VTube Collab") // the name Task Manager shows
        .set("ProductName", "VTube Collab")
        .set("LegalCopyright", "Copyright (c) 2026 Ben. MIT licence.")
        .set("OriginalFilename", file_name);
    res.compile().expect("couldn't build the icon into the program (needs the Windows SDK)");
}

/// The icon as a Windows .ico file with the sizes Windows asks for, 16 to 256 px,
/// each stored as a PNG image.
fn ico_file() -> Vec<u8> {
    const SIZES: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];
    let images: Vec<Vec<u8>> = SIZES.iter().map(|&size| png(size, &crate::icon::rgba(size))).collect();

    let mut out = vec![0, 0, 1, 0]; // reserved, type 1 = icon
    out.extend_from_slice(&(SIZES.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * SIZES.len() as u32;
    for (size, img) in SIZES.iter().zip(&images) {
        let side = if *size >= 256 { 0 } else { *size as u8 }; // 0 means 256
        out.extend_from_slice(&[side, side, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes()); // colour planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        out.extend_from_slice(&(img.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += img.len() as u32;
    }
    for img in images {
        out.extend_from_slice(&img);
    }
    out
}

/// RGBA pixels → PNG file.
fn png(size: u32, rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, size, size);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().expect("png header");
    writer.write_image_data(rgba).expect("png data");
    writer.finish().expect("png end");
    out
}
