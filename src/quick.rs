//! Contrôles rapides : Wi-Fi, Bluetooth et « Ne pas déranger », comme le centre de contrôle du
//! Mac (le volume est dans `volume`, la luminosité dans `brightness`).
//!
//! Tout se passe dans un fil à part : les API des radios et des appareils sont asynchrones, et
//! la recherche des réseaux prend du temps. Le fil relit l'état chaque seconde (et, pendant
//! qu'un panneau est ouvert, ce qu'il affiche) et prévient la barre par un message quand il change.

use std::{
    ffi::c_void,
    ptr::{null, null_mut},
    sync::{
        mpsc::{self, RecvTimeoutError},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};

use windows::{
    core::*,
    Devices::{Enumeration::*, Radios::*},
    Foundation::{Collections::IIterable, IReference},
    Win32::{
        Foundation::*,
        NetworkManagement::WiFi::*,
        System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED},
        UI::WindowsAndMessaging::PostMessageW,
    },
};

/// Protocoles Bluetooth (classique et basse consommation), pour retrouver les appareils associés.
pub(crate) const BLUETOOTH_PROTOCOLS: [&str; 2] = ["{e0cbf06c-cd8b-4647-bb8a-263b43f0f974}", "{bb7bb05e-5972-42b5-94fc-76eaa7084d49}"];
pub(crate) const IS_CONNECTED: &str = "System.Devices.Aep.IsConnected";

/// État des contrôles : `None` quand l'appareil n'en a pas (pas de Wi-Fi, pas de Bluetooth…).
#[derive(Clone, Copy, PartialEq, Default, Debug)]
pub struct State {
    pub wifi: Option<bool>,
    pub bluetooth: Option<bool>,
    /// « Ne pas déranger » : les notifications de Windows sont masquées.
    pub focus: Option<bool>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Toggle {
    Wifi,
    Bluetooth,
    Focus,
}

/// Un panneau des contrôles rapides : ce que le fil doit tenir à jour pendant qu'il est ouvert.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Panel {
    Wifi,
    Bluetooth,
    Sound,
    Brightness,
    Focus,
    Battery,
}

/// Réseau Wi-Fi à portée.
#[derive(Clone, PartialEq, Debug)]
pub struct Network {
    pub ssid: String,
    /// Qualité du signal, de 0 à 100.
    pub signal: u32,
    pub secured: bool,
    pub connected: bool,
    /// Profil enregistré (vide pour un réseau inconnu) : de quoi s'y connecter en un clic.
    pub profile: String,
}

/// Appareil Bluetooth associé.
#[derive(Clone, PartialEq, Debug)]
pub struct Device {
    pub name: String,
    pub connected: bool,
}

/// Ce que montrent les panneaux, relu seulement pendant qu'ils sont ouverts.
#[derive(Clone, PartialEq, Default, Debug)]
pub struct Details {
    pub networks: Vec<Network>,
    pub devices: Vec<Device>,
    /// Nom de la sortie audio par défaut.
    pub output: Option<String>,
}

enum Command {
    Toggle(Toggle),
    Watch(Option<Panel>),
    Connect(String),
}

#[derive(Default)]
struct Shared {
    state: State,
    details: Details,
}

pub struct Quick {
    shared: Arc<Mutex<Shared>>,
    commands: mpsc::Sender<Command>,
}

impl Quick {
    /// Démarre le suivi ; `message` est posté à `notify` à chaque changement.
    pub fn start(notify: HWND, message: u32) -> Self {
        let shared = Arc::new(Mutex::new(Shared::default()));
        let (commands, rx) = mpsc::channel();
        let worker_shared = Arc::clone(&shared);
        let notify = notify.0 as isize;
        thread::spawn(move || worker(worker_shared, rx, notify, message));
        Self { shared, commands }
    }

    pub fn state(&self) -> State {
        self.shared.lock().map(|s| s.state).unwrap_or_default()
    }

    pub fn details(&self) -> Details {
        self.shared.lock().map(|s| s.details.clone()).unwrap_or_default()
    }

    /// Bascule un contrôle. L'état affiché change tout de suite, sans attendre le système.
    pub fn toggle(&self, toggle: Toggle) {
        if let Ok(mut s) = self.shared.lock() {
            let value = match toggle {
                Toggle::Wifi => &mut s.state.wifi,
                Toggle::Bluetooth => &mut s.state.bluetooth,
                Toggle::Focus => &mut s.state.focus,
            };
            if let Some(on) = value.as_mut() {
                *on = !*on;
            }
        }
        let _ = self.commands.send(Command::Toggle(toggle));
    }

    /// Le panneau ouvert (ou aucun) : le fil tient son contenu à jour.
    pub fn watch(&self, panel: Option<Panel>) {
        let _ = self.commands.send(Command::Watch(panel));
    }

    /// Se connecte à un réseau Wi-Fi enregistré.
    pub fn connect(&self, profile: &str) {
        let _ = self.commands.send(Command::Connect(profile.to_owned()));
    }
}

fn worker(shared: Arc<Mutex<Shared>>, commands: mpsc::Receiver<Command>, notify: isize, message: u32) {
    unsafe {
        let _ = RoInitialize(RO_INIT_MULTITHREADED);
    }
    // Sans cette autorisation, Windows refuse de changer l'état des radios.
    let _ = Radio::RequestAccessAsync().and_then(|op| op.get());
    let wlan = Wlan::open();
    let mut radios: Vec<Radio> = Vec::new();
    let mut watched: Option<Panel> = None;
    let mut tick = 0u32;
    loop {
        // La liste des radios change rarement (clé Bluetooth branchée…) : relue toutes les 30 s.
        if tick.is_multiple_of(30) {
            radios = Radio::GetRadiosAsync().and_then(|op| op.get()).map(|list| list.into_iter().collect()).unwrap_or_default();
        }
        tick = tick.wrapping_add(1);
        match commands.recv_timeout(Duration::from_millis(1000)) {
            Ok(Command::Toggle(Toggle::Focus)) => {
                if let Some(on) = focus() {
                    set_focus(!on);
                }
            }
            Ok(Command::Toggle(toggle)) => {
                let kind = if toggle == Toggle::Wifi { RadioKind::WiFi } else { RadioKind::Bluetooth };
                if let Some(radio) = radios.iter().find(|r| r.Kind().ok() == Some(kind)) {
                    let next = if radio.State().ok() == Some(RadioState::On) { RadioState::Off } else { RadioState::On };
                    let _ = radio.SetStateAsync(next).and_then(|op| op.get());
                }
            }
            Ok(Command::Watch(panel)) => {
                watched = panel;
                // On ouvre le panneau du Wi-Fi : Windows recherche les réseaux à portée.
                if panel == Some(Panel::Wifi) {
                    if let Some(w) = &wlan {
                        w.scan();
                    }
                }
            }
            Ok(Command::Connect(profile)) => {
                if let Some(w) = &wlan {
                    w.connect(&profile);
                }
            }
            Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => {}
        }
        let radio = |kind: RadioKind| {
            let radio = radios.iter().find(|r| r.Kind().ok() == Some(kind))?;
            Some(radio.State().ok()? == RadioState::On)
        };
        let state = State { wifi: radio(RadioKind::WiFi), bluetooth: radio(RadioKind::Bluetooth), focus: focus() };
        let previous = shared.lock().map(|s| s.details.clone()).unwrap_or_default();
        let details = match watched {
            Some(Panel::Wifi) if state.wifi == Some(true) => Details { networks: wlan.as_ref().map(Wlan::networks).unwrap_or_default(), ..previous },
            Some(Panel::Wifi) => Details { networks: Vec::new(), ..previous },
            Some(Panel::Bluetooth) => Details { devices: bluetooth_devices(), ..previous },
            Some(Panel::Sound) => Details { output: crate::volume::output_name(), ..previous },
            _ => previous,
        };
        let Ok(mut s) = shared.lock() else { return };
        if s.state != state || s.details != details {
            s.state = state;
            s.details = details;
            drop(s);
            unsafe {
                let _ = PostMessageW(HWND(notify as _), message, WPARAM(0), LPARAM(0));
            }
        }
    }
}

/// Accès au Wi-Fi par l'API WLAN : la première carte sans fil.
struct Wlan {
    handle: HANDLE,
    interface: GUID,
}

impl Wlan {
    fn open() -> Option<Self> {
        unsafe {
            let (mut version, mut handle) = (0u32, HANDLE::default());
            if WlanOpenHandle(2, None, &mut version, &mut handle) != 0 {
                return None;
            }
            let mut list: *mut WLAN_INTERFACE_INFO_LIST = null_mut();
            let interface = if WlanEnumInterfaces(handle, None, &mut list) == 0 && !list.is_null() {
                let found = ((*list).dwNumberOfItems > 0).then(|| (*list).InterfaceInfo[0].InterfaceGuid);
                WlanFreeMemory(list as *const _);
                found
            } else {
                None
            };
            match interface {
                Some(interface) => Some(Self { handle, interface }),
                None => {
                    WlanCloseHandle(handle, None);
                    None
                }
            }
        }
    }

    fn scan(&self) {
        unsafe {
            WlanScan(self.handle, &self.interface, None, None, None);
        }
    }

    /// Réseaux à portée, une ligne par nom : le réseau connecté d'abord, puis du plus fort au
    /// plus faible.
    fn networks(&self) -> Vec<Network> {
        let mut out: Vec<Network> = Vec::new();
        unsafe {
            let mut list: *mut WLAN_AVAILABLE_NETWORK_LIST = null_mut();
            if WlanGetAvailableNetworkList(self.handle, &self.interface, 0, None, &mut list) != 0 || list.is_null() {
                return out;
            }
            let items = std::slice::from_raw_parts((*list).Network.as_ptr(), (*list).dwNumberOfItems as usize);
            for item in items {
                let len = (item.dot11Ssid.uSSIDLength as usize).min(item.dot11Ssid.ucSSID.len());
                let ssid = String::from_utf8_lossy(&item.dot11Ssid.ucSSID[..len]).into_owned();
                // Réseau masqué : il n'a pas de nom à montrer.
                if ssid.is_empty() {
                    continue;
                }
                let profile_len = item.strProfileName.iter().position(|&c| c == 0).unwrap_or(item.strProfileName.len());
                let network = Network {
                    ssid,
                    signal: item.wlanSignalQuality,
                    secured: item.bSecurityEnabled.as_bool(),
                    connected: item.dwFlags & WLAN_AVAILABLE_NETWORK_CONNECTED != 0,
                    profile: String::from_utf16_lossy(&item.strProfileName[..profile_len]),
                };
                match out.iter_mut().find(|n| n.ssid == network.ssid) {
                    Some(n) => {
                        n.signal = n.signal.max(network.signal);
                        n.connected |= network.connected;
                        if n.profile.is_empty() {
                            n.profile = network.profile;
                        }
                    }
                    None => out.push(network),
                }
            }
            WlanFreeMemory(list as *const _);
        }
        out.sort_by(|a, b| b.connected.cmp(&a.connected).then(b.signal.cmp(&a.signal)));
        out
    }

    fn connect(&self, profile: &str) {
        let profile = HSTRING::from(profile);
        let params = WLAN_CONNECTION_PARAMETERS {
            wlanConnectionMode: wlan_connection_mode_profile,
            strProfile: PCWSTR(profile.as_ptr()),
            pDot11Ssid: null_mut(),
            pDesiredBssidList: null_mut(),
            dot11BssType: dot11_BSS_type_any,
            dwFlags: 0,
        };
        unsafe {
            WlanConnect(self.handle, &self.interface, &params, None);
        }
    }
}

/// Appareils Bluetooth associés : les connectés d'abord, puis par nom.
fn bluetooth_devices() -> Vec<Device> {
    let mut out: Vec<Device> = Vec::new();
    for protocol in BLUETOOTH_PROTOCOLS {
        let aqs = HSTRING::from(format!(
            "System.Devices.Aep.ProtocolId:=\"{protocol}\" AND System.Devices.Aep.IsPaired:=System.StructuredQueryType.Boolean#True"
        ));
        let Ok(properties) = IIterable::<HSTRING>::try_from(vec![HSTRING::from(IS_CONNECTED)]) else { continue };
        let found = DeviceInformation::FindAllAsyncWithKindAqsFilterAndAdditionalProperties(&aqs, &properties, DeviceInformationKind::AssociationEndpoint)
            .and_then(|op| op.get());
        let Ok(found) = found else { continue };
        for device in found {
            let name = device.Name().map(|n| n.to_string()).unwrap_or_default();
            if name.is_empty() {
                continue;
            }
            let connected = device
                .Properties()
                .and_then(|p| p.Lookup(&HSTRING::from(IS_CONNECTED)))
                .and_then(|v| v.cast::<IReference<bool>>())
                .and_then(|v| v.Value())
                .unwrap_or(false);
            match out.iter_mut().find(|d| d.name == name) {
                Some(d) => d.connected |= connected,
                None => out.push(Device { name, connected }),
            }
        }
    }
    out.sort_by(|a, b| b.connected.cmp(&a.connected).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    out
}

/// État de Windows qui porte « Ne pas déranger » (WNF_SHEL_QUIETHOURS_ACTIVE_PROFILE_CHANGED) :
/// 0 quand il est coupé, 1 quand seules les notifications prioritaires passent (ce que fait le
/// bouton de Windows 11), 2 pour les seules alarmes. Windows n'a pas d'API publique pour ça.
const QUIET_HOURS: u64 = 0x0d83_063e_a3bf_1c75;

#[link(name = "ntdll", kind = "raw-dylib")]
extern "system" {
    fn NtQueryWnfStateData(name: *const u64, type_id: *const c_void, scope: *const c_void, stamp: *mut u32, buffer: *mut c_void, size: *mut u32) -> i32;
    fn NtUpdateWnfStateData(name: *const u64, buffer: *const c_void, size: u32, type_id: *const c_void, scope: *const c_void, matching: u32, check: u32) -> i32;
}

/// « Ne pas déranger » est actif (`None` si Windows ne le dit pas).
fn focus() -> Option<bool> {
    let (mut value, mut size, mut stamp) = (0u32, 4u32, 0u32);
    let status = unsafe { NtQueryWnfStateData(&QUIET_HOURS, null(), null(), &mut stamp, (&mut value as *mut u32).cast(), &mut size) };
    (status >= 0).then_some(size >= 4 && value != 0)
}

fn set_focus(on: bool) {
    let value = on as u32;
    unsafe {
        NtUpdateWnfStateData(&QUIET_HOURS, (&value as *const u32).cast(), 4, null(), null(), 0, 0);
    }
}
