//! Publishing our own Spout2 sender, so OBS (or anything Spout-aware) can pick it up.
//!
//! This is the reverse of reading a sender:
//!   1. Make a GPU texture that other programs are allowed to open ("shared").
//!   2. Write its handle, size and format into a shared-memory block named after the sender.
//!   3. Add the sender's name to Spout's "SpoutSenderNames" list.
//!   4. Every frame: copy the new pixels into the texture.
//!   5. On exit (`Drop`): remove the name from the list again.
//!
//! Spout guards its shared blocks with named Windows "mutexes" (locks that work across
//! programs). We take the same locks so we never write while another program reads.

use std::collections::BTreeSet;
use std::ffi::CString;
use std::mem::size_of;

use windows::core::{Interface, PCSTR, Result};
use windows::Win32::Foundation::{CloseHandle, E_FAIL, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11RenderTargetView, ID3D11Texture2D, D3D11_BIND_RENDER_TARGET,
    D3D11_BIND_SHADER_RESOURCE, D3D11_RESOURCE_MISC_SHARED, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::IDXGIResource;
use windows::Win32::System::LibraryLoader::GetModuleFileNameA;
use windows::Win32::System::Threading::{CreateMutexA, ReleaseMutex, WaitForSingleObject};

use crate::grab::create_device;
use crate::spout::{c_bytes_to_string, SharedMemory, SharedTextureInfo, MAX_SENDERS, NAME_SLOT_LEN};

const SENDER_LIST: &str = "SpoutSenderNames";

/// A named lock shared between programs. Unlocks automatically via `MutexGuard`.
struct NamedMutex(HANDLE);

impl NamedMutex {
    /// Create the lock, or open it if another program already made it.
    fn new(name: &str) -> Result<NamedMutex> {
        let c_name = CString::new(name).map_err(|_| windows::core::Error::from(E_FAIL))?;
        let handle = unsafe { CreateMutexA(None, false, PCSTR(c_name.as_ptr() as *const u8))? };
        Ok(NamedMutex(handle))
    }

    /// Wait up to `ms` milliseconds for the lock. `None` = timed out.
    fn lock(&self, ms: u32) -> Option<MutexGuard<'_>> {
        let r = unsafe { WaitForSingleObject(self.0, ms) };
        // WAIT_ABANDONED = the previous owner crashed while holding it; we own it now.
        (r == WAIT_OBJECT_0 || r == WAIT_ABANDONED).then_some(MutexGuard(self))
    }
}

impl Drop for NamedMutex {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Holding one of these = holding the lock. Dropping it unlocks.
/// The `'a` ("lifetime") tells Rust the guard can't outlive the mutex it came from.
struct MutexGuard<'a>(&'a NamedMutex);

impl Drop for MutexGuard<'_> {
    fn drop(&mut self) {
        unsafe {
            let _ = ReleaseMutex(self.0 .0);
        }
    }
}

/// Our own Spout2 sender.
pub struct SpoutSender {
    name: String,
    width: u32,
    height: u32,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    texture: ID3D11Texture2D,
    /// Lets the GPU draw directly into our shared texture (used by gpu::Unpacker).
    target: ID3D11RenderTargetView,
    access_lock: NamedMutex,
    // Kept alive so the sender stays visible; Windows deletes these blocks when closed.
    _info_mem: SharedMemory,
    _info_lock: NamedMutex,
}

impl SpoutSender {
    /// Create a sender called `name` with an RGBA texture of `width` × `height`.
    pub fn new(name: &str, width: u32, height: u32) -> Result<SpoutSender> {
        if name.is_empty() || name.len() >= NAME_SLOT_LEN {
            return Err(windows::core::Error::new(E_FAIL, "sender name must be 1–255 bytes"));
        }
        let (device, context) = create_device()?;

        // 1. A GPU texture other programs may open (MiscFlags = SHARED).
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: D3D11_RESOURCE_MISC_SHARED.0 as u32,
        };
        let mut texture: Option<ID3D11Texture2D> = None;
        let share_handle = unsafe {
            device.CreateTexture2D(&desc, None, Some(&mut texture))?;
            // `cast` asks the texture for a different "view" of itself that can share.
            let resource: IDXGIResource = texture.as_ref().unwrap().cast()?;
            resource.GetSharedHandle()?
        };
        let texture = texture.unwrap();

        // 2. The per-sender info block: handle, size, format, and our .exe path.
        let info_lock = NamedMutex::new(&format!("{name}_mutex"))?;
        let mut info_mem = SharedMemory::create(name, size_of::<SharedTextureInfo>())
            .ok_or_else(|| windows::core::Error::new(E_FAIL, "couldn't create sender info"))?;
        let mut info = SharedTextureInfo {
            // Spout's shared handles fit in 32 bits by design.
            share_handle: share_handle.0 as usize as u32,
            width,
            height,
            format: DXGI_FORMAT_R8G8B8A8_UNORM.0 as u32,
            usage: 0,
            description: [0; 256],
            partner_id: 0,
        };
        unsafe {
            // Leave the last byte as 0 so the path is always zero-terminated.
            GetModuleFileNameA(None, &mut info.description[..255]);
        }
        {
            let _guard = info_lock.lock(100);
            let bytes = info_mem.bytes_mut();
            // Copy our struct's bytes into the shared block.
            let src = unsafe {
                std::slice::from_raw_parts(&info as *const _ as *const u8, size_of::<SharedTextureInfo>())
            };
            bytes[..src.len()].copy_from_slice(src);
        }

        // 3. Add our name to Spout's sender list.
        update_sender_list(|names| {
            names.insert(name.to_string());
        })?;

        let access_lock = NamedMutex::new(&format!("{name}_SpoutAccessMutex"))?;

        let target = crate::gpu::rtv(&device, &texture)?;

        Ok(SpoutSender {
            name: name.to_string(),
            width,
            height,
            device,
            context,
            texture,
            target,
            access_lock,
            _info_mem: info_mem,
            _info_lock: info_lock,
        })
    }

    /// Copy one RGBA frame (width × height × 4 bytes, no padding) into the shared texture.
    pub fn send(&mut self, rgba: &[u8]) -> Result<()> {
        let row_bytes = self.width * 4;
        if rgba.len() < (row_bytes * self.height) as usize {
            return Err(windows::core::Error::new(E_FAIL, "frame is smaller than the sender"));
        }
        // If a receiver holds the lock for more than ~1 frame, skip this frame.
        let Some(_guard) = self.access_lock.lock(16) else {
            return Ok(());
        };
        unsafe {
            self.context
                .UpdateSubresource(&self.texture, 0, None, rgba.as_ptr() as *const _, row_bytes, 0);
            // Make sure the GPU has the new pixels before we release the lock.
            self.context.Flush();
        }
        Ok(())
    }
}

impl SpoutSender {
    /// The GPU connection this sender's texture lives on. GPU work that draws into
    /// the texture (e.g. gpu::Unpacker) must be created on this same device.
    pub fn device(&self) -> &ID3D11Device {
        &self.device
    }

    /// Let `draw` render directly into the shared texture, under Spout's access lock.
    /// `draw` gets the GPU command context and the texture's render target.
    pub fn render(
        &mut self,
        draw: impl FnOnce(&ID3D11DeviceContext, &ID3D11RenderTargetView) -> Result<()>,
    ) -> Result<()> {
        // If a receiver holds the lock for more than ~1 frame, skip this frame.
        let Some(_guard) = self.access_lock.lock(16) else {
            return Ok(());
        };
        draw(&self.context, &self.target)?;
        // Make sure the GPU has finished before we release the lock.
        unsafe { self.context.Flush() };
        Ok(())
    }
}

impl Drop for SpoutSender {
    fn drop(&mut self) {
        let name = self.name.clone();
        let _ = update_sender_list(|names| {
            names.remove(&name);
        });
    }
}

/// Read Spout's sender list, let `change` edit it, and write it back — all under Spout's lock.
/// Spout keeps the list sorted, so we use a `BTreeSet` (a sorted set) too.
fn update_sender_list(change: impl FnOnce(&mut BTreeSet<String>)) -> Result<()> {
    let lock = NamedMutex::new(&format!("{SENDER_LIST}_mutex"))?;
    let _guard = lock.lock(100);

    let mut mem = SharedMemory::create(SENDER_LIST, MAX_SENDERS * NAME_SLOT_LEN)
        .ok_or_else(|| windows::core::Error::new(E_FAIL, "couldn't open Spout sender list"))?;
    let slots = (mem.bytes().len() / NAME_SLOT_LEN).min(MAX_SENDERS);

    let mut names: BTreeSet<String> = mem.bytes()[..slots * NAME_SLOT_LEN]
        .chunks_exact(NAME_SLOT_LEN)
        .map(c_bytes_to_string)
        .take_while(|n| !n.is_empty())
        .collect();

    change(&mut names);

    let bytes = mem.bytes_mut();
    for (i, slot) in bytes[..slots * NAME_SLOT_LEN].chunks_exact_mut(NAME_SLOT_LEN).enumerate() {
        slot.fill(0);
        if let Some(n) = names.iter().nth(i) {
            slot[..n.len()].copy_from_slice(n.as_bytes());
        }
    }
    Ok(())
}
