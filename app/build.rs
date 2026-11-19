//! Build step for VTubeCollab.exe: icon and program name (see build_icon.rs).

#[path = "src/icon.rs"]
mod icon;
mod build_icon;

fn main() {
    build_icon::embed("VTubeCollab.exe");
}
