//! `SARP Launcher.lnk` inside the GTA folder, pointing at the installed exe,
//! so players can start SARP from where they already start the game.
//!
//! Saving over an existing link repoints it, so this is idempotent; nothing
//! outside `game_dir` is touched. Best-effort: a protected folder (Program
//! Files without elevation) simply gets no shortcut.

use std::path::{Path, PathBuf};

use crate::config::IS_DEV;

const FILE_NAME: &str = "SARP Launcher.lnk";
const DESCRIPTION: &str = "San Andreas Roleplay Launcher";

pub async fn ensure_game_folder_shortcut(game_dir: &Path) {
    if IS_DEV {
        return; // don't litter dev machines with shortcuts
    }
    let Ok(target) = std::env::current_exe() else {
        return;
    };
    let game_dir = game_dir.to_path_buf();
    let result = tauri::async_runtime::spawn_blocking(move || {
        write_link(&link_path(&game_dir), &target, &game_dir)
    })
    .await;
    match result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => log::info!("shortcut not created: {error}"),
        Err(error) => log::warn!("shortcut task failed: {error}"),
    }
}

/// Writes a `.lnk` with IShellLinkW. Blocking (COM).
pub fn write_link(link: &Path, target: &Path, cwd: &Path) -> windows::core::Result<()> {
    use windows::core::{Interface, HSTRING};
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, IPersistFile, CLSCTX_INPROC_SERVER,
        COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};

    unsafe {
        // S_OK/S_FALSE must be balanced by CoUninitialize; RPC_E_CHANGED_MODE
        // means the thread already has COM (other model) — fine, don't undo.
        let init = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let result = (|| {
            let shell_link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
            let target = HSTRING::from(target.as_os_str());
            shell_link.SetPath(&target)?;
            shell_link.SetWorkingDirectory(&HSTRING::from(cwd.as_os_str()))?;
            shell_link.SetDescription(&HSTRING::from(DESCRIPTION))?;
            shell_link.SetIconLocation(&target, 0)?;
            let file: IPersistFile = shell_link.cast()?;
            file.Save(&HSTRING::from(link.as_os_str()), true)
        })();
        if init.is_ok() {
            CoUninitialize();
        }
        result
    }
}

fn link_path(game_dir: &Path) -> PathBuf {
    game_dir.join(FILE_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_and_repoints_a_shortcut() {
        let dir = tempfile::tempdir().unwrap();
        let link = link_path(dir.path());
        let first = std::env::current_exe().unwrap();
        write_link(&link, &first, dir.path()).unwrap();
        let size_a = std::fs::metadata(&link).unwrap().len();
        assert!(size_a > 0);

        // Saving again over the same file just repoints it.
        let notepad = PathBuf::from(r"C:\Windows\System32\notepad.exe");
        write_link(&link, &notepad, dir.path()).unwrap();
        let bytes = std::fs::read(&link).unwrap();
        // .lnk header: HeaderSize 0x4C, then the ShellLink CLSID.
        assert_eq!(&bytes[..4], &[0x4C, 0, 0, 0]);
        let wide: Vec<u8> = "notepad.exe"
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect();
        let ascii = b"notepad.exe";
        let contains = |needle: &[u8]| {
            bytes
                .windows(needle.len())
                .any(|w| w.eq_ignore_ascii_case(needle))
        };
        assert!(
            contains(&wide) || contains(ascii),
            "link does not point at notepad"
        );
    }
}
