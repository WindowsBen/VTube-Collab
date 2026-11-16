//! Grabbing one frame from a Spout2 sender and saving it as a PNG.
//!
//! The sender's image lives on the graphics card (GPU) as a "texture". Steps:
//!   1. Create our own connection to the GPU (a Direct3D 11 "device").
//!   2. Open the sender's shared texture using the handle Spout published.
//!   3. GPU textures can't be read by the CPU directly, so copy it into a
//!      "staging" texture, which can be.
//!   4. Read the pixels row by row and save them as a PNG, transparency included.

use windows::core::Result;
use windows::Win32::Foundation::{HANDLE, HMODULE};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAPPED_SUBRESOURCE,
    D3D11_MAP_READ, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_B8G8R8A8_UNORM_SRGB,
    DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_FORMAT_R8G8B8A8_UNORM_SRGB,
};

use crate::spout::SharedTextureInfo;

/// A captured frame: plain RGBA bytes, 4 per pixel, row after row.
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Copy the current image of a Spout sender from the GPU into normal memory.
///
/// `Result<Frame>` means: either `Ok(frame)` or `Err(error)`. The `?` after a call
/// means "if this failed, stop here and hand the error back to whoever called us".
pub fn grab_frame(info: &SharedTextureInfo) -> Result<Frame> {
    unsafe {
        // 1. Connect to the default graphics card.
        let mut device: Option<ID3D11Device> = None;
        let mut context: Option<ID3D11DeviceContext> = None;
        D3D11CreateDevice(
            None,                     // default graphics card
            D3D_DRIVER_TYPE_HARDWARE, // real GPU, not software emulation
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )?;
        // `unwrap()` = "this must be there". D3D11CreateDevice succeeded, so it is.
        let device = device.unwrap();
        let context = context.unwrap();

        // 2. Open the sender's texture through the handle it shared.
        let handle = HANDLE(info.share_handle as usize as *mut _);
        let mut shared: Option<ID3D11Texture2D> = None;
        device.OpenSharedResource(handle, &mut shared)?;
        let shared = shared.unwrap();

        // Read the texture's real description (size, format...).
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        shared.GetDesc(&mut desc);

        // 3. Make a CPU-readable copy with the same size and format.
        let staging_desc = D3D11_TEXTURE2D_DESC {
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
            MipLevels: 1,
            ArraySize: 1,
            ..desc // `..desc` = copy every other field from `desc`
        };
        let mut staging: Option<ID3D11Texture2D> = None;
        device.CreateTexture2D(&staging_desc, None, Some(&mut staging))?;
        let staging = staging.unwrap();

        context.CopyResource(&staging, &shared);

        // 4. Read the pixels.
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;

        let width = desc.Width as usize;
        let height = desc.Height as usize;
        let row_bytes = width * 4;
        let mut rgba = vec![0u8; row_bytes * height];

        // The GPU may pad each row, so step by `RowPitch`, not by `row_bytes`.
        let src = mapped.pData as *const u8;
        for y in 0..height {
            let src_row =
                std::slice::from_raw_parts(src.add(y * mapped.RowPitch as usize), row_bytes);
            rgba[y * row_bytes..(y + 1) * row_bytes].copy_from_slice(src_row);
        }

        context.Unmap(&staging, 0);

        // BGRA formats store blue first; swap to RGBA so the PNG has the right colours.
        if is_bgra(desc.Format) {
            for px in rgba.chunks_exact_mut(4) {
                px.swap(0, 2);
            }
        } else if !is_rgba(desc.Format) {
            return Err(windows::core::Error::new(
                windows::Win32::Foundation::E_NOTIMPL,
                format!("pixel format {} isn't supported yet", desc.Format.0),
            ));
        }

        Ok(Frame { width: desc.Width, height: desc.Height, rgba })
    }
}

fn is_bgra(f: DXGI_FORMAT) -> bool {
    f == DXGI_FORMAT_B8G8R8A8_UNORM || f == DXGI_FORMAT_B8G8R8A8_UNORM_SRGB
}

fn is_rgba(f: DXGI_FORMAT) -> bool {
    f == DXGI_FORMAT_R8G8B8A8_UNORM || f == DXGI_FORMAT_R8G8B8A8_UNORM_SRGB
}
