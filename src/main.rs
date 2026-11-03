// En release, pas de console : l'application vit dans la zone de notification.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod anim;
mod app;
mod autostart;
mod brightness;
mod canvas;
mod claude;
mod media;
mod dock;
mod launcher;
mod live;
mod quick;
mod shell;
mod snapbar;
mod settings;
mod taskbar;
mod volume;
mod stages;
mod win;

use windows::{
    core::w,
    Win32::{Foundation::*, System::Threading::CreateMutexW},
};

fn main() -> windows::core::Result<()> {
    // Une seule instance à la fois ; la relancer ouvre ses réglages.
    let _mutex = unsafe { CreateMutexW(None, true, w!("Local\\StageManagerForWindows"))? };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        app::show_running_settings();
        return Ok(());
    }

    // Quoi qu'il arrive, ne jamais laisser des fenêtres réduites derrière nous.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        app::restore_hidden();
        default_hook(info);
    }));

    autostart::follow_exe();
    app::run()
}
