//! Volume du son : celui de la sortie audio par défaut, comme le curseur de Windows.

use windows::Win32::{
    Media::Audio::{eConsole, eRender, Endpoints::IAudioEndpointVolume, IMMDeviceEnumerator, MMDeviceEnumerator},
    System::Com::{CoCreateInstance, CLSCTX_ALL},
};

/// Sortie audio par défaut. Ouverte à chaque fois : elle change quand on branche un casque.
fn endpoint() -> Option<IAudioEndpointVolume> {
    unsafe {
        let devices: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
        let device = devices.GetDefaultAudioEndpoint(eRender, eConsole).ok()?;
        device.Activate(CLSCTX_ALL, None).ok()
    }
}

/// Volume (entre 0 et 1) et coupure du son.
pub fn get() -> Option<(f32, bool)> {
    let e = endpoint()?;
    unsafe { Some((e.GetMasterVolumeLevelScalar().ok()?, e.GetMute().ok()?.as_bool())) }
}

/// Change le volume ; monter le son le rétablit s'il était coupé.
pub fn set(level: f32) {
    let Some(e) = endpoint() else { return };
    unsafe {
        let _ = e.SetMasterVolumeLevelScalar(level.clamp(0.0, 1.0), std::ptr::null());
        if level > 0.0 {
            let _ = e.SetMute(false, std::ptr::null());
        }
    }
}

pub fn toggle_mute() {
    let Some(e) = endpoint() else { return };
    unsafe {
        let muted = e.GetMute().map(|m| m.as_bool()).unwrap_or(false);
        let _ = e.SetMute(!muted, std::ptr::null());
    }
}
