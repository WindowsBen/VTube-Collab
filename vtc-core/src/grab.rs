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

use std::sync::Mutex;

use windows::core::{Interface, Result};
use windows::Win32::Foundation::{E_INVALIDARG, E_NOTIMPL, HANDLE, HMODULE, LUID};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_UNKNOWN};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter, IDXGIAdapter1, IDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE,
};
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
        // 1 + 2. Connect to the graphics card the sender's image is on, and open it
        // through the handle it shared.
        let (device, context, shared) = open_shared(info.share_handle)?;
        unsafe {

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

// ---------------- choosing the graphics card ----------------
//
// Laptops often have two graphics cards (e.g. Intel + NVIDIA). An image shared on
// one card can't be opened on the other ("The parameter is incorrect"). So we open
// the avatar on whichever card it lives on, remember that card, and create
// everything else (partner outputs, group source) on it too.

/// The card the avatar was found on, if it isn't Windows' default.
/// (A `Mutex` lets several threads read/write it safely.)
static CHOSEN_CARD: Mutex<Option<LUID>> = Mutex::new(None);

/// Connect to the graphics card in use (the avatar's, see `open_shared`; otherwise
/// Windows' default). Returns the device (creates GPU objects) and its context
/// (gives the GPU commands). Shared with the Spout sender.
pub fn create_device() -> Result<(ID3D11Device, ID3D11DeviceContext)> {
    let chosen = *CHOSEN_CARD.lock().unwrap();
    match chosen.and_then(find_card) {
        Some(card) => device_on(Some(&card)),
        None => device_on(None),
    }
}

/// Open a texture another program shared (a Spout sender's image) on whichever
/// graphics card it lives on. Returns that card's device and context, and the texture.
pub fn open_shared(share_handle: u32) -> Result<(ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D)> {
    let handle = HANDLE(share_handle as usize as *mut _);
    // Usually the card we already use (or Windows' default) is the right one.
    let (device, context) = create_device()?;
    if let Some(texture) = open_on(&device, handle) {
        return Ok((device, context, texture));
    }
    // Otherwise try every real graphics card.
    for card in hardware_cards() {
        let Ok((device, context)) = device_on(Some(&card)) else { continue };
        if let Some(texture) = open_on(&device, handle) {
            if let Ok(desc) = unsafe { card.GetDesc1() } {
                *CHOSEN_CARD.lock().unwrap() = Some(desc.AdapterLuid);
            }
            return Ok((device, context, texture));
        }
    }
    Err(windows::core::Error::new(
        E_INVALIDARG,
        "its image can't be read on any graphics card. If this PC has two graphics \
         cards, set your VTubing app, OBS and VTube Collab to \"High performance\" in Windows \
         Settings > System > Display > Graphics, then restart them.",
    ))
}

/// Name of the graphics card in use, for the log file.
pub fn card_in_use() -> String {
    let chosen = *CHOSEN_CARD.lock().unwrap();
    let card = match chosen.and_then(find_card) {
        Some(card) => Some(card),
        // Windows' default is the first card in its list.
        None => unsafe { CreateDXGIFactory1::<IDXGIFactory1>().and_then(|f| f.EnumAdapters1(0)).ok() },
    };
    card.and_then(|c| unsafe { c.GetDesc1() }.ok())
        .map(|d| {
            let end = d.Description.iter().position(|&c| c == 0).unwrap_or(d.Description.len());
            String::from_utf16_lossy(&d.Description[..end])
        })
        .unwrap_or_else(|| "unknown".into())
}

/// Create a device on `card`, or on Windows' default card if `None`.
fn device_on(card: Option<&IDXGIAdapter1>) -> Result<(ID3D11Device, ID3D11DeviceContext)> {
    let adapter: Option<IDXGIAdapter> = match card {
        Some(c) => Some(c.cast()?),
        None => None,
    };
    let mut device: Option<ID3D11Device> = None;
    let mut context: Option<ID3D11DeviceContext> = None;
    unsafe {
        D3D11CreateDevice(
            adapter.as_ref(),
            // A specific card needs "UNKNOWN"; the default needs "HARDWARE" (a real GPU,
            // not software emulation).
            if adapter.is_some() { D3D_DRIVER_TYPE_UNKNOWN } else { D3D_DRIVER_TYPE_HARDWARE },
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

/// Try to open the shared texture on `device`; `None` if it lives on another card.
fn open_on(device: &ID3D11Device, handle: HANDLE) -> Option<ID3D11Texture2D> {
    let mut texture: Option<ID3D11Texture2D> = None;
    unsafe { device.OpenSharedResource(handle, &mut texture) }.ok()?;
    texture
}

/// The real graphics cards (skips Windows' software "Basic Render Driver").
fn hardware_cards() -> Vec<IDXGIAdapter1> {
    let mut cards = Vec::new();
    unsafe {
        let Ok(factory) = CreateDXGIFactory1::<IDXGIFactory1>() else { return cards };
        let mut i = 0;
        while let Ok(card) = factory.EnumAdapters1(i) {
            let software = card.GetDesc1().map(|d| d.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0);
            if software == Ok(false) {
                cards.push(card);
            }
            i += 1;
        }
    }
    cards
}

/// The card with this ID ("LUID"), if it's still there.
fn find_card(id: LUID) -> Option<IDXGIAdapter1> {
    hardware_cards().into_iter().find(|c| unsafe { c.GetDesc1() }.is_ok_and(|d| d.AdapterLuid == id))
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
