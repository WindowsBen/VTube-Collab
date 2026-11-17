//! Facts about this PC, for the log file (helps make sense of testers' reports).

use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};

/// Names of the graphics cards ("adapters"), in Windows' order. The first one is
/// the default, which our capture/output code uses.
pub fn graphics_cards() -> Vec<String> {
    let mut names = Vec::new();
    unsafe {
        let Ok(factory) = CreateDXGIFactory1::<IDXGIFactory1>() else { return names };
        let mut i = 0;
        while let Ok(adapter) = factory.EnumAdapters1(i) {
            if let Ok(desc) = adapter.GetDesc1() {
                let end = desc.Description.iter().position(|&c| c == 0).unwrap_or(desc.Description.len());
                let name = String::from_utf16_lossy(&desc.Description[..end]);
                let vram_mb = desc.DedicatedVideoMemory / (1024 * 1024);
                names.push(format!("{name} ({vram_mb} MB)"));
            }
            i += 1;
        }
    }
    names
}
