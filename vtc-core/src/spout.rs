//! Reading Spout2's shared-memory bookkeeping.
//!
//! How Spout2 keeps track of senders:
//!   * A shared block of Windows memory called "SpoutSenderNames" holds a list of
//!     sender names. Each name gets a fixed 256-byte slot; an empty slot ends the list.
//!   * For every sender, another shared block — named exactly like the sender — holds
//!     a small `SharedTextureInfo` record: width, height, pixel format, GPU handle, etc.
//!
//! We only *read* these blocks. Nothing is changed.

use std::ffi::CString;
use std::mem::size_of;

use windows::core::PCSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::System::Memory::{
    CreateFileMappingA, MapViewOfFile, OpenFileMappingA, UnmapViewOfFile, VirtualQuery,
    FILE_MAP_ALL_ACCESS, FILE_MAP_READ, MEMORY_BASIC_INFORMATION, MEMORY_MAPPED_VIEW_ADDRESS,
    PAGE_READWRITE,
};

/// Each sender name occupies a fixed slot of this many bytes in "SpoutSenderNames".
pub(crate) const NAME_SLOT_LEN: usize = 256;

/// Spout's default maximum number of senders (the list holds this many slots).
pub(crate) const MAX_SENDERS: usize = 64;

/// Layout of the per-sender record, copied from Spout2's C++ `SharedTextureInfo`.
/// `#[repr(C)]` tells Rust to lay it out in memory exactly like C/C++ would.
/// `pub` makes it visible to other files (modules) in this project.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SharedTextureInfo {
    pub share_handle: u32,      // GPU texture handle the sender shares
    pub width: u32,             // image width in pixels
    pub height: u32,            // image height in pixels
    pub format: u32,            // pixel format (a DXGI_FORMAT number)
    pub usage: u32,             // unused by us
    pub description: [u8; 256], // newer Spout versions store the sender's .exe path here
    pub partner_id: u32,        // unused by us
}

/// A view of a named shared-memory block (read-only via `open`, read-write via `create`).
/// When this value goes out of scope, `Drop` below releases the Windows resources.
/// `pub(crate)` = usable by other files in this library, but not from outside it.
pub(crate) struct SharedMemory {
    handle: HANDLE,
    view: MEMORY_MAPPED_VIEW_ADDRESS,
    len: usize,
}

impl SharedMemory {
    /// Create the block called `name` (or open it read-write if it already exists).
    pub(crate) fn create(name: &str, size: usize) -> Option<SharedMemory> {
        let c_name = CString::new(name).ok()?;
        unsafe {
            let handle = CreateFileMappingA(
                INVALID_HANDLE_VALUE, // backed by memory, not a file
                None,
                PAGE_READWRITE,
                0,
                size as u32,
                PCSTR(c_name.as_ptr() as *const u8),
            )
            .ok()?;
            let view = MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, 0);
            if view.Value.is_null() {
                let _ = CloseHandle(handle);
                return None;
            }
            let mut info = MEMORY_BASIC_INFORMATION::default();
            VirtualQuery(Some(view.Value), &mut info, size_of::<MEMORY_BASIC_INFORMATION>());
            Some(SharedMemory { handle, view, len: info.RegionSize })
        }
    }

    /// The block's contents as a writable byte slice (only valid for `create`d blocks).
    pub(crate) fn bytes_mut(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.view.Value as *mut u8, self.len) }
    }

    /// Open the shared-memory block called `name`. Returns `None` if it doesn't exist.
    pub(crate) fn open(name: &str) -> Option<SharedMemory> {
        // Windows wants a C-style string (ends with a zero byte).
        let c_name = CString::new(name).ok()?;

        // `unsafe` = we're calling into Windows directly, so Rust can't check it for us.
        unsafe {
            let handle =
                OpenFileMappingA(FILE_MAP_READ.0, false, PCSTR(c_name.as_ptr() as *const u8))
                    .ok()?;

            // Map the whole block into our program's memory, read-only.
            let view = MapViewOfFile(handle, FILE_MAP_READ, 0, 0, 0);
            if view.Value.is_null() {
                let _ = CloseHandle(handle);
                return None;
            }

            // Ask Windows how big the mapped region is, so we never read past the end.
            let mut info = MEMORY_BASIC_INFORMATION::default();
            VirtualQuery(Some(view.Value), &mut info, size_of::<MEMORY_BASIC_INFORMATION>());

            Some(SharedMemory { handle, view, len: info.RegionSize })
        }
    }

    /// The block's contents as a byte slice.
    pub(crate) fn bytes(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.view.Value as *const u8, self.len) }
    }
}

impl Drop for SharedMemory {
    fn drop(&mut self) {
        unsafe {
            let _ = UnmapViewOfFile(self.view);
            let _ = CloseHandle(self.handle);
        }
    }
}

/// Turn a zero-terminated byte buffer into a Rust `String`.
pub fn c_bytes_to_string(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// Read every sender name from the "SpoutSenderNames" list.
pub fn read_sender_names() -> Vec<String> {
    let Some(list) = SharedMemory::open("SpoutSenderNames") else {
        return Vec::new(); // no Spout program has ever run since boot
    };

    list.bytes()
        .chunks_exact(NAME_SLOT_LEN)
        .map(c_bytes_to_string)
        .take_while(|name| !name.is_empty()) // first empty slot = end of list
        .collect()
}

/// Read the size/format record for one sender.
pub fn read_sender_info(name: &str) -> Option<SharedTextureInfo> {
    let mem = SharedMemory::open(name)?;
    let bytes = mem.bytes();
    if bytes.len() < size_of::<SharedTextureInfo>() {
        return None;
    }
    // Copy the bytes into our struct. `read_unaligned` is safe for any byte position.
    Some(unsafe { std::ptr::read_unaligned(bytes.as_ptr() as *const SharedTextureInfo) })
}

/// Human-readable name for the most common pixel formats Spout senders use.
pub fn format_name(format: u32) -> &'static str {
    match format {
        0 => "default (BGRA 8-bit)",
        10 => "RGBA 16-bit float",
        24 => "RGB10 A2",
        28 => "RGBA 8-bit",
        29 => "RGBA 8-bit sRGB",
        87 => "BGRA 8-bit",
        91 => "BGRA 8-bit sRGB",
        _ => "other",
    }
}
