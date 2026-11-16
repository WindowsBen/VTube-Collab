//! spout-probe: lists every Spout2 sender (video source) currently running on this PC.
//!
//! How Spout2 keeps track of senders:
//!   * A shared block of Windows memory called "SpoutSenderNames" holds a list of
//!     sender names. Each name gets a fixed 256-byte slot; an empty slot ends the list.
//!   * For every sender, another shared block — named exactly like the sender — holds
//!     a small `SharedTextureInfo` record: width, height, pixel format, etc.
//!
//! We only *read* these blocks. Nothing is changed, and no video is touched.

use std::ffi::CString;
use std::mem::size_of;

use windows::core::PCSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Memory::{
    MapViewOfFile, OpenFileMappingA, UnmapViewOfFile, VirtualQuery, FILE_MAP_READ,
    MEMORY_BASIC_INFORMATION, MEMORY_MAPPED_VIEW_ADDRESS,
};

/// Each sender name occupies a fixed slot of this many bytes in "SpoutSenderNames".
const NAME_SLOT_LEN: usize = 256;

/// Layout of the per-sender record, copied from Spout2's C++ `SharedTextureInfo`.
/// `#[repr(C)]` tells Rust to lay it out in memory exactly like C/C++ would.
#[repr(C)]
#[derive(Clone, Copy)]
struct SharedTextureInfo {
    share_handle: u32,     // GPU texture handle the sender shares
    width: u32,            // image width in pixels
    height: u32,           // image height in pixels
    format: u32,           // pixel format (a DXGI_FORMAT number)
    usage: u32,            // unused by us
    description: [u8; 256], // newer Spout versions store the sender's .exe path here
    partner_id: u32,       // unused by us
}

/// A read-only view of a named shared-memory block.
/// When this value goes out of scope, `Drop` below releases the Windows resources.
struct SharedMemory {
    handle: HANDLE,
    view: MEMORY_MAPPED_VIEW_ADDRESS,
    len: usize,
}

impl SharedMemory {
    /// Open the shared-memory block called `name`. Returns `None` if it doesn't exist.
    fn open(name: &str) -> Option<SharedMemory> {
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
    fn bytes(&self) -> &[u8] {
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
fn c_bytes_to_string(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// Read every sender name from the "SpoutSenderNames" list.
fn read_sender_names() -> Vec<String> {
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
fn read_sender_info(name: &str) -> Option<SharedTextureInfo> {
    let mem = SharedMemory::open(name)?;
    let bytes = mem.bytes();
    if bytes.len() < size_of::<SharedTextureInfo>() {
        return None;
    }
    // Copy the bytes into our struct. `read_unaligned` is safe for any byte position.
    Some(unsafe { std::ptr::read_unaligned(bytes.as_ptr() as *const SharedTextureInfo) })
}

/// Human-readable name for the most common pixel formats Spout senders use.
fn format_name(format: u32) -> &'static str {
    match format {
        0 => "default (BGRA 8-bit)",
        10 => "RGBA 16-bit float",
        24 => "RGB10 A2",
        28 => "RGBA 8-bit",
        87 => "BGRA 8-bit",
        _ => "other",
    }
}

fn main() {
    let names = read_sender_names();

    if names.is_empty() {
        println!("No Spout2 senders found.");
        println!("Tip: open your VTubing app and turn on its Spout2 output, then run this again.");
        return;
    }

    println!("Found {} Spout2 sender(s):\n", names.len());
    for name in &names {
        match read_sender_info(name) {
            Some(info) => {
                println!("  • {name}");
                println!("      size:   {} × {}", info.width, info.height);
                println!("      format: {} ({})", format_name(info.format), info.format);
                let app = c_bytes_to_string(&info.description);
                if !app.is_empty() {
                    println!("      app:    {app}");
                }
            }
            None => println!("  • {name}  (listed, but its details couldn't be read — it may have just closed)"),
        }
    }
}
