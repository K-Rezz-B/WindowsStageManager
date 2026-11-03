//! Luminosité de l'écran intégré (portable), réglée comme le curseur de Windows : par le
//! réglage « Luminosité de l'affichage » du mode d'alimentation actif. Sa valeur actuelle
//! arrive, elle, en avis d'alimentation (voir `app::activity`).
//!
//! Appliquer un mode d'alimentation prend quelques millisecondes : un fil à part s'en charge, et
//! ne garde que la dernière valeur demandée quand on fait glisser le curseur.

use std::{
    ptr::null_mut,
    sync::{
        mpsc::{self, Sender},
        Mutex, OnceLock,
    },
    thread,
};

use windows::{
    core::GUID,
    Win32::{
        Foundation::{LocalFree, HLOCAL},
        System::{
            Power::{PowerGetActiveScheme, PowerSetActiveScheme, PowerWriteACValueIndex, PowerWriteDCValueIndex},
            SystemServices::GUID_VIDEO_SUBGROUP,
        },
    },
};

/// « Luminosité de l'affichage » (VIDEONORMALLEVEL), en pour cent.
const VIDEO_NORMAL_LEVEL: GUID = GUID::from_u128(0xaded5e82_b909_4619_9949_f5d71dac0bcb);

/// Règle la luminosité (de 0 à 100), sur secteur comme sur batterie.
pub fn set(percent: u32) {
    static WORKER: OnceLock<Mutex<Sender<u32>>> = OnceLock::new();
    let worker = WORKER.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<u32>();
        thread::spawn(move || {
            while let Ok(mut percent) = rx.recv() {
                // Seule la dernière valeur compte.
                while let Ok(next) = rx.try_recv() {
                    percent = next;
                }
                apply(percent);
            }
        });
        Mutex::new(tx)
    });
    if let Ok(tx) = worker.lock() {
        let _ = tx.send(percent.min(100));
    }
}

fn apply(percent: u32) {
    unsafe {
        let mut scheme: *mut GUID = null_mut();
        if PowerGetActiveScheme(None, &mut scheme).is_err() || scheme.is_null() {
            return;
        }
        let _ = PowerWriteACValueIndex(None, scheme, Some(&GUID_VIDEO_SUBGROUP), Some(&VIDEO_NORMAL_LEVEL), percent);
        let _ = PowerWriteDCValueIndex(None, scheme, Some(&GUID_VIDEO_SUBGROUP), Some(&VIDEO_NORMAL_LEVEL), percent);
        // Réappliquer le mode le fait prendre effet tout de suite.
        let _ = PowerSetActiveScheme(None, Some(scheme));
        let _ = LocalFree(HLOCAL(scheme.cast()));
    }
}
