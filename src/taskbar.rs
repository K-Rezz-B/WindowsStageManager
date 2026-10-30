//! Barre des tâches de Windows en masquage automatique tant que le Dock est affiché.
//! C'est un réglage de Windows : on le remet comme avant en quittant.

use std::{
    mem::size_of,
    sync::atomic::{AtomicBool, Ordering},
};

use windows::{
    core::w,
    Win32::{
        Foundation::*,
        UI::{Shell::*, WindowsAndMessaging::FindWindowW},
    },
};

/// Vrai si c'est nous qui avons activé le masquage (et qu'il faudra donc le retirer).
static CHANGED: AtomicBool = AtomicBool::new(false);

fn appbar_data() -> APPBARDATA {
    APPBARDATA {
        cbSize: size_of::<APPBARDATA>() as u32,
        hWnd: unsafe { FindWindowW(w!("Shell_TrayWnd"), None) }.unwrap_or_default(),
        ..Default::default()
    }
}

fn set_state(autohide: bool) {
    let mut abd = appbar_data();
    unsafe {
        let state = SHAppBarMessage(ABM_GETSTATE, &mut abd) as u32;
        let flags = if autohide { state | ABS_AUTOHIDE } else { state & !ABS_AUTOHIDE };
        abd.lParam = LPARAM(flags as isize);
        SHAppBarMessage(ABM_SETSTATE, &mut abd);
    }
}

fn is_autohide() -> bool {
    let mut abd = appbar_data();
    unsafe { SHAppBarMessage(ABM_GETSTATE, &mut abd) as u32 & ABS_AUTOHIDE != 0 }
}

/// Active le masquage automatique, sauf s'il l'est déjà (on n'y touchera alors pas en partant).
pub fn autohide() {
    if !is_autohide() {
        set_state(true);
        CHANGED.store(true, Ordering::SeqCst);
    }
}

/// Remet la barre des tâches comme on l'a trouvée.
pub fn restore() {
    if CHANGED.swap(false, Ordering::SeqCst) {
        set_state(false);
    }
}
