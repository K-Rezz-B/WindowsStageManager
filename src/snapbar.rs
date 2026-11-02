//! Barre d'ancrage de Windows 11 (« Snap Layouts ») qui descend quand on fait glisser une
//! fenêtre en haut de l'écran : elle prendrait la place de la zone de dépôt de l'encoche.
//! C'est un réglage de Windows (Paramètres > Système > Multitâche > Ancrer les fenêtres). On ne
//! fait que l'écrire : l'Explorateur ne le relit qu'à son démarrage, c'est-à-dire à la prochaine
//! ouverture de session (le décocher dans les Paramètres, lui, s'applique tout de suite).
//! La valeur d'origine est notée dans `%APPDATA%\StageManager\snapbar.txt`, pour la rendre
//! quand on désactive la fonctionnalité. En quittant, on n'y touche pas.

use std::path::{Path, PathBuf};

use windows::{
    core::*,
    Win32::{Foundation::*, System::Registry::*},
};

const KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Advanced");
const VALUE: PCWSTR = w!("EnableSnapBar");
/// Ce que le fichier retient quand la valeur n'existait pas (réglage par défaut : affichée).
const ABSENT: &str = "absente";

fn read() -> Option<u32> {
    let mut value = 0u32;
    let mut size = size_of::<u32>() as u32;
    let err = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            KEY,
            VALUE,
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut core::ffi::c_void),
            Some(&mut size),
        )
    };
    (err == ERROR_SUCCESS).then_some(value)
}

fn write(value: Option<u32>) {
    unsafe {
        let _ = match value {
            Some(v) => RegSetKeyValueW(
                HKEY_CURRENT_USER,
                KEY,
                VALUE,
                REG_DWORD.0,
                Some(&v as *const u32 as *const core::ffi::c_void),
                size_of::<u32>() as u32,
            ),
            None => RegDeleteKeyValueW(HKEY_CURRENT_USER, KEY, VALUE),
        };
    }
}

fn file() -> Option<PathBuf> {
    Some(Path::new(&std::env::var("APPDATA").ok()?).join("StageManager").join("snapbar.txt"))
}

/// Coupe la barre d'ancrage, sauf si elle l'est déjà (on n'y touchera alors pas en partant).
pub fn disable() {
    let current = read();
    if current == Some(0) {
        return;
    }
    let Some(file) = file() else { return };
    // Déjà noté (coupée par nous, puis rallumée à la main) : on garde la vraie valeur d'origine.
    if !file.exists() {
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&file, current.map_or(ABSENT.to_owned(), |v| v.to_string()));
    }
    write(Some(0));
}

/// Rend à la barre d'ancrage le réglage qu'on lui a trouvé.
pub fn restore() {
    let Some(file) = file() else { return };
    let Ok(text) = std::fs::read_to_string(&file) else { return };
    let _ = std::fs::remove_file(&file);
    let original = match text.trim() {
        ABSENT => None,
        v => Some(v.parse().unwrap_or(1)),
    };
    if read() != original {
        write(original);
    }
}
