//! Build step for the launcher: the same icon and program name as the app.

#[path = "../app/src/icon.rs"]
mod icon;
#[path = "../app/build_icon.rs"]
mod build_icon;

fn main() {
    build_icon::embed("VTube Collab.exe");
}
