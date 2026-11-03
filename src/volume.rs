//! Volume du son : celui de la sortie audio par défaut, comme le curseur de Windows.

use windows::Win32::{
    Devices::FunctionDiscovery::PKEY_Device_FriendlyName,
    Media::Audio::{eConsole, eRender, Endpoints::IAudioEndpointVolume, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator},
    System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL, STGM_READ},
};

/// Sortie audio par défaut. Ouverte à chaque fois : elle change quand on branche un casque.
fn endpoint() -> Option<IAudioEndpointVolume> {
    unsafe { device()?.Activate(CLSCTX_ALL, None).ok() }
}

fn device() -> Option<IMMDevice> {
    unsafe {
        let devices: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
        devices.GetDefaultAudioEndpoint(eRender, eConsole).ok()
    }
}

/// Nom de la sortie audio par défaut (« Haut-parleurs (Realtek(R) Audio) »).
pub fn output_name() -> Option<String> {
    unsafe {
        let value = device()?.OpenPropertyStore(STGM_READ).ok()?.GetValue(&PKEY_Device_FriendlyName).ok()?;
        Some(value.to_string()).filter(|name| !name.is_empty())
    }
}

/// Sortie audio par défaut et son identifiant, pour la suivre sans la rouvrir à chaque lecture
/// du volume (voir `live`).
pub fn default_endpoint() -> Option<(String, IAudioEndpointVolume)> {
    unsafe {
        let device = device()?;
        let raw = device.GetId().ok()?;
        let id = raw.to_string().unwrap_or_default();
        CoTaskMemFree(Some(raw.0 as _));
        Some((id, device.Activate(CLSCTX_ALL, None).ok()?))
    }
}

/// Volume (entre 0 et 1) et coupure du son.
pub fn get() -> Option<(f32, bool)> {
    read(&endpoint()?)
}

pub fn read(e: &IAudioEndpointVolume) -> Option<(f32, bool)> {
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
