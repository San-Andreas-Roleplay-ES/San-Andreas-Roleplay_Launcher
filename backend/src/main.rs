// Release builds are GUI-subsystem binaries: no console window, nothing on stdout.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    sarp_launcher_lib::run();
}
