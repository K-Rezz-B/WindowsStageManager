//! Activités en direct de l'encoche, côté système : le volume qui change, le micro et la
//! caméra en service, un appareil Bluetooth qui se connecte.
//!
//! Tout se passe dans un fil à part, qui relit le volume et le micro à intervalles réguliers et
//! prévient l'encoche par un message quand ils changent. Les appareils Bluetooth, eux, sont
//! suivis par un observateur de Windows, qui ne réveille l'appli qu'à un changement. La batterie, la luminosité et la touche de
//! verrouillage des majuscules arrivent, elles, en messages Windows (voir `app::activity`).

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use windows::{
    core::*,
    Devices::Enumeration::*,
    Foundation::{Collections::IIterable, IReference, TypedEventHandler},
    Win32::{
        Foundation::*,
        Media::Audio::Endpoints::IAudioEndpointVolume,
        System::{
            Registry::*,
            WinRT::{RoInitialize, RO_INIT_MULTITHREADED},
        },
        UI::WindowsAndMessaging::PostMessageW,
    },
};

/// Le fil relit le volume toutes les 100 ms ; le micro, la caméra et la sortie audio par
/// défaut, moins souvent (en nombre de tours).
const TICK: Duration = Duration::from_millis(100);
const PRIVACY_EVERY: u32 = 10;
const OUTPUT_EVERY: u32 = 20;

/// Où Windows note les applis qui se servent du micro et de la caméra : une sous-clé par appli
/// (et, sous `NonPackaged`, une par programme classique). `LastUsedTimeStop` vaut 0 tant
/// qu'elle s'en sert.
const CONSENT_STORE: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\CapabilityAccessManager\\ConsentStore";

/// Ce qui vient de se passer.
#[derive(Clone, PartialEq, Debug)]
pub enum Event {
    Volume { level: f32, muted: bool },
    /// Une appli commence à se servir de la caméra (ou du micro).
    Sensor { camera: bool, app: String },
    Bluetooth(String),
}

/// Applis qui se servent en ce moment du micro et de la caméra.
#[derive(Clone, PartialEq, Default, Debug)]
pub struct Privacy {
    pub microphone: Vec<String>,
    pub camera: Vec<String>,
}

#[derive(Default)]
struct Shared {
    events: Vec<Event>,
    privacy: Privacy,
}

pub struct Live {
    shared: Arc<Mutex<Shared>>,
}

impl Live {
    /// Démarre le suivi ; `message` est posté à `notify` à chaque changement.
    pub fn start(notify: HWND, message: u32) -> Self {
        let shared = Arc::new(Mutex::new(Shared::default()));
        let worker_shared = Arc::clone(&shared);
        let notify = notify.0 as isize;
        thread::spawn(move || worker(worker_shared, notify, message));
        Self { shared }
    }

    /// Ce qui s'est passé depuis la dernière fois.
    pub fn take(&self) -> Vec<Event> {
        self.shared.lock().map(|mut s| std::mem::take(&mut s.events)).unwrap_or_default()
    }

    pub fn privacy(&self) -> Privacy {
        self.shared.lock().map(|s| s.privacy.clone()).unwrap_or_default()
    }
}

/// Ajoute des nouvelles et prévient l'encoche.
fn post(shared: &Mutex<Shared>, notify: isize, message: u32, events: Vec<Event>, privacy: Option<&Privacy>) {
    let Ok(mut s) = shared.lock() else { return };
    s.events.extend(events);
    if let Some(p) = privacy {
        s.privacy = p.clone();
    }
    drop(s);
    unsafe {
        let _ = PostMessageW(HWND(notify as _), message, WPARAM(0), LPARAM(0));
    }
}

fn worker(shared: Arc<Mutex<Shared>>, notify: isize, message: u32) {
    unsafe {
        let _ = RoInitialize(RO_INIT_MULTITHREADED);
    }
    // Gardé en vie tant que le fil tourne.
    let _watcher = watch_bluetooth(Arc::clone(&shared), notify, message);
    let mut endpoint: Option<(String, IAudioEndpointVolume)> = None;
    let mut volume: Option<(f32, bool)> = None;
    let mut privacy: Option<Privacy> = None;
    let mut tick = 0u32;
    loop {
        tick = tick.wrapping_add(1);
        let mut events = Vec::new();
        let mut changed = false;

        // La sortie par défaut change quand on branche un casque : son volume n'est pas un
        // changement de volume.
        if tick.is_multiple_of(OUTPUT_EVERY) || endpoint.is_none() {
            let next = crate::volume::default_endpoint();
            if next.as_ref().map(|e| &e.0) != endpoint.as_ref().map(|e| &e.0) {
                volume = next.as_ref().and_then(|e| crate::volume::read(&e.1));
                endpoint = next;
            }
        }
        if let Some((_, e)) = &endpoint {
            let now = crate::volume::read(e);
            if let (Some((level, muted)), Some((before, was_muted))) = (now, volume) {
                if (level - before).abs() > 0.001 || muted != was_muted {
                    events.push(Event::Volume { level, muted });
                }
            }
            volume = now.or(volume);
        }

        if tick.is_multiple_of(PRIVACY_EVERY) {
            let now = Privacy { microphone: in_use("microphone"), camera: in_use("webcam") };
            if let Some(before) = &privacy {
                for app in now.camera.iter().filter(|a| !before.camera.contains(a)) {
                    events.push(Event::Sensor { camera: true, app: app.clone() });
                }
                // Une visio prend les deux : la caméra suffit.
                for app in now.microphone.iter().filter(|a| !before.microphone.contains(a) && !now.camera.contains(a)) {
                    events.push(Event::Sensor { camera: false, app: app.clone() });
                }
            }
            if privacy.as_ref() != Some(&now) {
                changed = true;
                privacy = Some(now);
            }
        }

        if changed || !events.is_empty() {
            post(&shared, notify, message, events, privacy.as_ref());
        }
        thread::sleep(TICK);
    }
}

/// Appareils Bluetooth associés, par identifiant : leur nom et s'ils sont connectés.
#[derive(Default)]
struct Devices {
    known: HashMap<String, (String, bool)>,
    /// Le premier inventaire est fini : ce qui change ensuite est nouveau.
    ready: bool,
}

/// Suit les appareils Bluetooth associés (classiques et basse consommation) : un appareil qui
/// passe à « connecté » est une nouvelle.
fn watch_bluetooth(shared: Arc<Mutex<Shared>>, notify: isize, message: u32) -> Option<DeviceWatcher> {
    let protocols: Vec<String> = crate::quick::BLUETOOTH_PROTOCOLS.iter().map(|p| format!("System.Devices.Aep.ProtocolId:=\"{p}\"")).collect();
    let aqs = HSTRING::from(format!("({}) AND System.Devices.Aep.IsPaired:=System.StructuredQueryType.Boolean#True", protocols.join(" OR ")));
    let properties = IIterable::<HSTRING>::try_from(vec![HSTRING::from(crate::quick::IS_CONNECTED)]).ok()?;
    let watcher = DeviceInformation::CreateWatcherWithKindAqsFilterAndAdditionalProperties(&aqs, &properties, DeviceInformationKind::AssociationEndpoint).ok()?;
    let devices = Arc::new(Mutex::new(Devices::default()));
    let connected_in = |properties: windows::core::Result<windows::Foundation::Collections::IMapView<HSTRING, IInspectable>>| -> Option<bool> {
        properties.ok()?.Lookup(&HSTRING::from(crate::quick::IS_CONNECTED)).ok()?.cast::<IReference<bool>>().ok()?.Value().ok()
    };

    let d = Arc::clone(&devices);
    let added = TypedEventHandler::<DeviceWatcher, DeviceInformation>::new(move |_, info| {
        if let Some(info) = info {
            let (Ok(id), Ok(name)) = (info.Id(), info.Name()) else { return Ok(()) };
            let connected = connected_in(info.Properties()).unwrap_or(false);
            if let Ok(mut d) = d.lock() {
                d.known.insert(id.to_string(), (name.to_string(), connected));
            }
        }
        Ok(())
    });
    let d = Arc::clone(&devices);
    let updated = TypedEventHandler::<DeviceWatcher, DeviceInformationUpdate>::new(move |_, update| {
        let Some(update) = update else { return Ok(()) };
        let (Ok(id), Some(connected)) = (update.Id(), connected_in(update.Properties())) else { return Ok(()) };
        let news = {
            let Ok(mut d) = d.lock() else { return Ok(()) };
            let ready = d.ready;
            match d.known.get_mut(&id.to_string()) {
                Some((name, was)) => {
                    let news = ready && connected && !*was;
                    *was = connected;
                    news.then(|| name.clone())
                }
                None => None,
            }
        };
        if let Some(name) = news.filter(|n| !n.is_empty()) {
            post(&shared, notify, message, vec![Event::Bluetooth(name)], None);
        }
        Ok(())
    });
    let d = Arc::clone(&devices);
    let removed = TypedEventHandler::<DeviceWatcher, DeviceInformationUpdate>::new(move |_, update| {
        if let (Some(update), Ok(mut d)) = (update, d.lock()) {
            if let Ok(id) = update.Id() {
                d.known.remove(&id.to_string());
            }
        }
        Ok(())
    });
    let d = Arc::clone(&devices);
    let completed = TypedEventHandler::<DeviceWatcher, IInspectable>::new(move |_, _| {
        if let Ok(mut d) = d.lock() {
            d.ready = true;
        }
        Ok(())
    });
    watcher.Added(&added).ok()?;
    watcher.Updated(&updated).ok()?;
    watcher.Removed(&removed).ok()?;
    watcher.EnumerationCompleted(&completed).ok()?;
    watcher.Start().ok()?;
    Some(watcher)
}

/// Applis qui se servent en ce moment du micro (`microphone`) ou de la caméra (`webcam`).
fn in_use(capability: &str) -> Vec<String> {
    let base = format!("{CONSENT_STORE}\\{capability}");
    let mut out = Vec::new();
    for key in subkeys(&base) {
        if key == "NonPackaged" {
            let programs = format!("{base}\\NonPackaged");
            for program in subkeys(&programs) {
                if active(&format!("{programs}\\{program}")) {
                    out.push(app_name(&program));
                }
            }
        } else if active(&format!("{base}\\{key}")) {
            out.push(app_name(&key));
        }
    }
    out.sort();
    out.dedup();
    out
}

fn subkeys(path: &str) -> Vec<String> {
    let mut out = Vec::new();
    unsafe {
        let mut key = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, &HSTRING::from(path), 0, KEY_READ, &mut key).is_err() {
            return out;
        }
        let mut name = [0u16; 512];
        for i in 0.. {
            let mut len = name.len() as u32;
            if RegEnumKeyExW(key, i, PWSTR(name.as_mut_ptr()), &mut len, None, PWSTR::null(), None, None).is_err() {
                break;
            }
            out.push(String::from_utf16_lossy(&name[..len as usize]));
        }
        let _ = RegCloseKey(key);
    }
    out
}

/// L'appli s'en sert : elle a commencé (`LastUsedTimeStart`) sans avoir fini.
fn active(path: &str) -> bool {
    let read = |value: PCWSTR| -> Option<u64> {
        let (mut data, mut size) = (0u64, size_of::<u64>() as u32);
        unsafe {
            RegGetValueW(HKEY_CURRENT_USER, &HSTRING::from(path), value, RRF_RT_REG_QWORD, None, Some(&mut data as *mut u64 as _), Some(&mut size))
                .ok()
                .ok()?;
        }
        Some(data)
    };
    read(w!("LastUsedTimeStop")) == Some(0) && read(w!("LastUsedTimeStart")).is_some_and(|t| t != 0)
}

/// Nom lisible d'une appli d'après sa clé : `Microsoft.WindowsCamera_8wekyb3d8bbwe` donne
/// « WindowsCamera », `C:#Program Files#Google#Chrome#Application#chrome.exe` donne « Chrome ».
fn app_name(key: &str) -> String {
    let name = if key.contains('#') {
        let file = key.rsplit('#').next().unwrap_or(key);
        file.rsplit_once('.').map_or(file, |(stem, _)| stem)
    } else {
        let package = key.split('_').next().unwrap_or(key);
        package.rsplit('.').next().unwrap_or(package)
    };
    match name.to_lowercase().as_str() {
        "msedge" | "msedgewebview2" => "Edge".to_owned(),
        "ms-teams" | "msteams" | "teams" => "Teams".to_owned(),
        "windowscamera" => "Caméra".to_owned(),
        _ => {
            let mut chars = name.chars();
            chars.next().map_or_else(String::new, |c| c.to_uppercase().chain(chars).collect())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noms_des_applis() {
        assert_eq!(app_name("C:#Program Files#Google#Chrome#Application#chrome.exe"), "Chrome");
        assert_eq!(app_name("C:#Program Files (x86)#Microsoft#Edge#Application#msedge.exe"), "Edge");
        assert_eq!(app_name("MSTeams_8wekyb3d8bbwe"), "Teams");
        assert_eq!(app_name("Microsoft.WindowsCamera_8wekyb3d8bbwe"), "Caméra");
        assert_eq!(app_name("C:#Users#me#AppData#Local#Discord#app-1.0#Discord.exe"), "Discord");
        assert_eq!(app_name("C:#zoom#bin#zoom.exe"), "Zoom");
    }
}
