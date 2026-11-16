//! Grabbing frames from a Spout2 sender.
//!
//! The sender's image lives on the graphics card (GPU) as a "texture". Steps:
//!   1. Create our own connection to the GPU (a Direct3D 11 "device").
//!   2. Open the sender's shared texture using the handle Spout published.
//!   3. GPU textures can't be read by the CPU directly, so copy it into a
//!      "staging" texture, which can be.
//!   4. Read the pixels row by row into normal memory as RGBA.
//!
//! Steps 1–3 (setup) happen once in `Grabber::open`. Each `grab` call then only
//! does the copy + read, so it can run 60+ times a second.

use windows::core::Result;
use windows::Win32::Foundation::{E_NOTIMPL, HANDLE, HMODULE};
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

/// Everything needed to read one sender repeatedly.
/// The `_device` field is never used directly, but keeping it here keeps the GPU
/// connection alive for as long as the Grabber exists (the leading `_` tells Rust
/// "I know this isn't read").
pub struct Grabber {
    _device: ID3D11Device,
    context: ID3D11DeviceContext,
    shared: ID3D11Texture2D,
    staging: ID3D11Texture2D,
    pub width: u32,
    pub height: u32,
    bgra: bool,
    /// The Spout handle this Grabber was opened with. If the sender restarts or
    /// changes size, Spout publishes a new handle and we must reopen.
    pub share_handle: u32,
}

impl Grabber {
    /// One-time setup: connect to the GPU, open the sender's texture, make a staging copy.
    pub fn open(info: &SharedTextureInfo) -> Result<Grabber> {
        // 1. Connect to the default graphics card.
        let (device, context) = create_device()?;
        unsafe {
            // 2. Open the sender's texture through the handle it shared.
            let handle = HANDLE(info.share_handle as usize as *mut _);
            let mut shared: Option<ID3D11Texture2D> = None;
            device.OpenSharedResource(handle, &mut shared)?;
            let shared = shared.unwrap();

            // Read the texture's real description (size, format...).
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            shared.GetDesc(&mut desc);

            let bgra = is_bgra(desc.Format);
            if !bgra && !is_rgba(desc.Format) {
                return Err(windows::core::Error::new(
                    E_NOTIMPL,
                    format!("pixel format {} isn't supported yet", desc.Format.0),
                ));
            }

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

            Ok(Grabber {
                _device: device,
                context,
                shared,
                staging: staging.unwrap(),
                width: desc.Width,
                height: desc.Height,
                bgra,
                share_handle: info.share_handle,
            })
        }
    }

    /// Copy the sender's current image into `out` as RGBA (4 bytes per pixel).
    /// `out` is reused between calls so we don't allocate 8 MB every frame.
    /// `&mut self` = this method may change the Grabber; `&mut Vec<u8>` = it fills `out`.
    pub fn grab(&mut self, out: &mut Vec<u8>) -> Result<()> {
        let width = self.width as usize;
        let height = self.height as usize;
        let row_bytes = width * 4;
        out.resize(row_bytes * height, 0);

        unsafe {
            // GPU → staging copy. Map waits until the GPU has finished it.
            self.context.CopyResource(&self.staging, &self.shared);

            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            self.context
                .Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;

            // The GPU may pad each row, so step by `RowPitch`, not by `row_bytes`.
            let src = mapped.pData as *const u8;
            for y in 0..height {
                let src_row =
                    std::slice::from_raw_parts(src.add(y * mapped.RowPitch as usize), row_bytes);
                out[y * row_bytes..(y + 1) * row_bytes].copy_from_slice(src_row);
            }

            self.context.Unmap(&self.staging, 0);
        }

        // BGRA formats store blue first; swap to RGBA so colours come out right.
        if self.bgra {
            for px in out.chunks_exact_mut(4) {
                px.swap(0, 2);
            }
        }
        Ok(())
    }
}

/// Connect to the default graphics card. Returns the device (creates GPU objects)
/// and its context (gives the GPU commands). Shared with the Spout sender.
pub fn create_device() -> Result<(ID3D11Device, ID3D11DeviceContext)> {
    let mut device: Option<ID3D11Device> = None;
    let mut context: Option<ID3D11DeviceContext> = None;
    unsafe {
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
    }
    // `unwrap()` = "this must be there". D3D11CreateDevice succeeded, so it is.
    Ok((device.unwrap(), context.unwrap()))
}

/// A single captured frame: plain RGBA bytes, 4 per pixel, row after row.
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Shortcut for one-off captures: set up, grab once, done.
pub fn grab_frame(info: &SharedTextureInfo) -> Result<Frame> {
    let mut grabber = Grabber::open(info)?;
    let mut rgba = Vec::new();
    grabber.grab(&mut rgba)?;
    Ok(Frame { width: grabber.width, height: grabber.height, rgba })
}

fn is_bgra(f: DXGI_FORMAT) -> bool {
    f == DXGI_FORMAT_B8G8R8A8_UNORM || f == DXGI_FORMAT_B8G8R8A8_UNORM_SRGB
}

fn is_rgba(f: DXGI_FORMAT) -> bool {
    f == DXGI_FORMAT_R8G8B8A8_UNORM || f == DXGI_FORMAT_R8G8B8A8_UNORM_SRGB
}
