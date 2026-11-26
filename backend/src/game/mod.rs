//! The GTA San Andreas install and the running game: folder validation and
//! pickers, install detection, health check, launching, the folder shortcut.
//! (Mods have their own module: `crate::mods`.)

pub mod detect;
pub mod health;
pub mod launcher;
pub mod paths;
pub mod shortcut;
