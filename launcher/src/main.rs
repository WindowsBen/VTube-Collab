//! "VTube Collab.exe" at the top of the release folder: starts the real app,
//! bin\VTubeCollab.exe, and closes. The app has to live in bin\ next to the
//! GStreamer files it needs; this way people still get one clear program to
//! double-click, with the app's icon.

// No console window.
#![windows_subsystem = "windows"]

use std::path::PathBuf;
use std::process::Command;

fn main() {
    let app = std::env::current_exe()
        .ok()
        .and_then(|me| me.parent().map(|folder| folder.join("bin").join("VTubeCollab.exe")))
        .unwrap_or_else(|| PathBuf::from(r"bin\VTubeCollab.exe"));

    // Anything given to the launcher is passed on to the app.
    if let Err(e) = Command::new(&app).args(std::env::args_os().skip(1)).spawn() {
        show_error(&format!(
            "VTube Collab couldn't start.\n\n{}: {e}\n\n\
             Make sure you unzipped the whole folder and kept the \"bin\" folder next to this program.",
            app.display()
        ));
    }
}

/// A Windows message box (there's no console to print to).
fn show_error(text: &str) {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn MessageBoxW(window: *mut std::ffi::c_void, text: *const u16, title: *const u16, kind: u32) -> i32;
    }
    let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    const MB_ICONERROR: u32 = 0x10;
    // SAFETY: both strings are valid and end with a 0, as Windows expects.
    unsafe {
        MessageBoxW(std::ptr::null_mut(), wide(text).as_ptr(), wide("VTube Collab").as_ptr(), MB_ICONERROR);
    }
}
