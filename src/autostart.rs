//! Lancement automatique à l'ouverture de session (clé « Run » du registre de l'utilisateur).

use windows::{
    core::*,
    Win32::{Foundation::*, System::Registry::*},
};

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const VALUE: PCWSTR = w!("StageManager");

/// Commande enregistrée : le chemin de cet exécutable, entre guillemets.
fn command() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    Some(format!("\"{}\"", exe.display()))
}

fn read() -> Option<String> {
    let mut buf = [0u16; 1024];
    let mut size = (buf.len() * 2) as u32;
    let err = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            VALUE,
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size as *mut u32),
        )
    };
    // `size` compte le zéro final, en octets.
    (err == ERROR_SUCCESS).then(|| String::from_utf16_lossy(&buf[..(size as usize / 2).saturating_sub(1)]))
}

pub fn is_enabled() -> bool {
    read().is_some()
}

pub fn set(enabled: bool) {
    unsafe {
        if !enabled {
            let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, VALUE);
            return;
        }
        let Some(cmd) = command() else { return };
        let data: Vec<u16> = cmd.encode_utf16().chain(Some(0)).collect();
        let _ = RegSetKeyValueW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            VALUE,
            REG_SZ.0,
            Some(data.as_ptr().cast()),
            (data.len() * 2) as u32,
        );
    }
}

/// Si le lancement automatique est activé mais vise un ancien emplacement de l'exécutable,
/// le fait pointer ici. Seulement en release, pour qu'une version de développement ne prenne
/// pas la place de la vraie.
pub fn follow_exe() {
    if cfg!(debug_assertions) {
        return;
    }
    if read().is_some_and(|current| Some(current) != command()) {
        set(true);
    }
}
