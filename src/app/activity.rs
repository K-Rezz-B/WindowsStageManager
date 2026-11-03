//! Activités en direct de l'encoche, comme la Dynamic Island de l'iPhone : quand le volume ou
//! la luminosité changent, qu'on branche le chargeur, que la batterie faiblit, qu'un appareil
//! Bluetooth se connecte ou qu'on verrouille les majuscules, l'encoche fermée s'allonge vers le
//! bas un instant pour le montrer. Le micro et la caméra en service, et les majuscules
//! verrouillées, restent signalés par de petites marques dans l'encoche fermée.
//!
//! Les chats Claude Code ouverts y ont aussi chacun leur pastille : bleue en cours, orange quand
//! ils attendent une réponse, verte quand ils ont fini. Quand un chat pose une question ou
//! finit, l'encoche s'allonge pour le dire ; un clic dessus ramène ce chat au premier plan.
//! Ouverte, l'encoche les liste tous (voir `notch`).
//!
//! Le volume, le micro, la caméra et le Bluetooth viennent du fil de `live` ; la batterie et la
//! luminosité, des avis d'alimentation de Windows ; les majuscules, du clavier (entrée brute,
//! reçue même quand une autre appli a le clavier).

use std::f32::consts::PI;

use windows::Win32::{
    System::{
        Power::{GetSystemPowerStatus, RegisterPowerSettingNotification, POWERBROADCAST_SETTING, SYSTEM_POWER_STATUS},
        SystemServices::{GUID_ACDC_POWER_SOURCE, GUID_BATTERY_PERCENTAGE_REMAINING, GUID_VIDEO_CURRENT_MONITOR_BRIGHTNESS},
    },
    UI::Input::{GetRawInputData, RegisterRawInputDevices, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER, RIDEV_INPUTSINK, RID_INPUT, RIM_TYPEKEYBOARD},
};

use super::{
    quickbar::{draw_bluetooth, draw_volume, line},
    notch::CHAT_MAX,
    timer::draw_ring,
    *,
};
use crate::{
    canvas::round_corners,
    claude::{self, Chat, Claude, Place, Status},
    live::{Event, Live, Privacy},
};

pub(super) const WM_LIVE: u32 = WM_APP + 13;
/// La liste des chats Claude Code a changé.
const WM_CLAUDE: u32 = WM_APP + 14;
const TIMER_ACTIVITY: usize = 14;

/// Durée d'affichage : courte pour un réglage qu'on est en train de faire, plus longue pour
/// une nouvelle.
const BRIEF: u32 = 1500;
const NEWS: u32 = 2600;
/// Un chat Claude Code qui pose une question ou finit : le temps de lire son nom.
const CHAT_NEWS: u32 = 5000;
/// Seuils de la batterie faible, en pour cent.
pub(super) const LOW: [u32; 2] = [20, 10];

pub(super) const GREEN: (u8, u8, u8) = (48, 209, 88);
const ORANGE: (u8, u8, u8) = (255, 159, 10);
pub(super) const RED: (u8, u8, u8) = (255, 69, 58);
const BLUE: (u8, u8, u8) = (10, 132, 255);
/// Orange terre cuite du logo de Claude.
const CLAUDE: (u8, u8, u8) = (217, 119, 87);

#[derive(Clone, PartialEq, Debug)]
pub(super) enum Activity {
    Volume { level: f32, muted: bool },
    Brightness(f32),
    Charging(u32),
    LowBattery(u32),
    Bluetooth(String),
    Caps(bool),
    Sensor { camera: bool, app: String },
    /// Minuteur ou séance de Pomodoro (voir `timer`).
    Timer { title: String, detail: String, color: (u8, u8, u8) },
    /// Un chat Claude Code attend une réponse (`waiting`) ou vient de finir.
    Claude { name: String, waiting: bool, session: String },
}

/// Petite marque de l'encoche fermée.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum Mark {
    Caps,
    Camera,
    Microphone,
    /// Le logo de Claude, devant les pastilles des chats.
    Claude,
    Chat(Status),
}

pub(super) struct ActivityUi {
    live: Live,
    /// Activité montrée (gardée après coup, le temps que l'encoche se referme dessus).
    pub current: Option<Activity>,
    /// L'activité est encore à l'écran.
    pub active: bool,
    privacy: Privacy,
    caps: bool,
    caps_down: bool,
    /// Luminosité de l'écran intégré, charge de la batterie (en pour cent) et branchement sur
    /// secteur : `None` sans écran intégré ou sans batterie. Les contrôles rapides les montrent.
    pub brightness: Option<u32>,
    pub battery: Option<u32>,
    pub on_ac: Option<bool>,
    claude: Claude,
    /// Chats Claude Code ouverts, tels qu'à la dernière lecture.
    pub chats: Vec<Chat>,
    /// Démarrage : le premier avis de luminosité n'est qu'un état des lieux.
    started: Instant,
}

impl ActivityUi {
    pub fn new(hwnd: HWND) -> Self {
        let mut status = SYSTEM_POWER_STATUS::default();
        let has_battery = unsafe { GetSystemPowerStatus(&mut status) }.is_ok() && status.BatteryFlag != 128 && status.BatteryLifePercent <= 100;
        unsafe {
            // Windows envoie aussitôt l'état actuel de chaque réglage, puis chaque changement.
            for guid in [GUID_VIDEO_CURRENT_MONITOR_BRIGHTNESS, GUID_ACDC_POWER_SOURCE, GUID_BATTERY_PERCENTAGE_REMAINING] {
                let _ = RegisterPowerSettingNotification(HANDLE(hwnd.0), &guid, DEVICE_NOTIFY_WINDOW_HANDLE);
            }
            // Le clavier, même quand une autre appli l'a : pour la touche des majuscules.
            let keyboard = RAWINPUTDEVICE { usUsagePage: 1, usUsage: 6, dwFlags: RIDEV_INPUTSINK, hwndTarget: hwnd };
            let _ = RegisterRawInputDevices(&[keyboard], size_of::<RAWINPUTDEVICE>() as u32);
        }
        Self {
            live: Live::start(hwnd, WM_LIVE),
            current: None,
            active: false,
            privacy: Privacy::default(),
            caps: unsafe { GetKeyState(VK_CAPITAL.0 as i32) } & 1 != 0,
            caps_down: false,
            brightness: None,
            battery: has_battery.then_some(status.BatteryLifePercent as u32),
            on_ac: has_battery.then_some(status.ACLineStatus == 1),
            claude: Claude::start(hwnd, WM_CLAUDE),
            chats: Vec::new(),
            started: Instant::now(),
        }
    }

    /// Marques de l'encoche fermée, de gauche à droite.
    pub fn marks(&self) -> Vec<Mark> {
        let mut marks = Vec::new();
        if !self.chats.is_empty() {
            marks.push(Mark::Claude);
            marks.extend(self.chats.iter().take(CHAT_MAX).map(|c| Mark::Chat(c.status)));
        }
        if self.caps {
            marks.push(Mark::Caps);
        }
        if !self.privacy.camera.is_empty() {
            marks.push(Mark::Camera);
        }
        if !self.privacy.microphone.is_empty() {
            marks.push(Mark::Microphone);
        }
        marks
    }
}

impl App {
    /// Messages des activités : le fil de `live`, l'alimentation, le clavier, la fin d'une
    /// activité. Le clavier repart vers Windows (il le faut pour l'entrée brute).
    pub(super) fn handle_activity_message(&mut self, msg: u32, wp: WPARAM, lp: LPARAM) -> bool {
        match msg {
            WM_LIVE => {
                let privacy = self.activity.live.privacy();
                let marks = privacy != self.activity.privacy;
                self.activity.privacy = privacy;
                let events = self.activity.live.take();
                // Plusieurs à la fois : la dernière compte.
                let last = events.into_iter().last().map(|e| match e {
                    Event::Volume { level, muted } => (Activity::Volume { level, muted }, BRIEF),
                    Event::Sensor { camera, app } => (Activity::Sensor { camera, app }, NEWS),
                    Event::Bluetooth(name) => (Activity::Bluetooth(name), NEWS),
                });
                match last {
                    Some((activity, ms)) => self.show_activity(activity, ms),
                    None if marks => self.notch_retarget(),
                    None => {}
                }
                true
            }
            WM_CLAUDE => {
                self.on_chats();
                true
            }
            WM_POWERBROADCAST if wp.0 as u32 == PBT_POWERSETTINGCHANGE && lp.0 != 0 => {
                let setting = unsafe { &*(lp.0 as *const POWERBROADCAST_SETTING) };
                if setting.DataLength >= 4 {
                    let value = unsafe { std::ptr::read_unaligned(setting.Data.as_ptr() as *const u32) };
                    self.on_power(setting.PowerSetting, value);
                }
                true
            }
            WM_TIMER if wp.0 == TIMER_ACTIVITY => {
                unsafe {
                    let _ = KillTimer(self.hwnd, TIMER_ACTIVITY);
                }
                self.activity.active = false;
                self.notch_retarget();
                true
            }
            WM_INPUT => {
                self.on_raw_input(lp);
                false
            }
            _ => false,
        }
    }

    fn on_power(&mut self, setting: GUID, value: u32) {
        self.on_power_activity(setting, value);
        // La pastille des contrôles rapides montre la batterie et la luminosité.
        self.render_all_quick();
    }

    fn on_power_activity(&mut self, setting: GUID, value: u32) {
        let a = &mut self.activity;
        if setting == GUID_VIDEO_CURRENT_MONITOR_BRIGHTNESS {
            let before = a.brightness.replace(value);
            let fresh = before.is_none() && a.started.elapsed() > Duration::from_secs(3);
            if before.is_some_and(|b| b != value) || fresh {
                self.show_activity(Activity::Brightness(value.min(100) as f32 / 100.0), BRIEF);
            }
        } else if setting == GUID_ACDC_POWER_SOURCE {
            let on_ac = value == 0;
            if a.on_ac.replace(on_ac) == Some(false) && on_ac {
                let percent = a.battery.unwrap_or(0);
                self.show_activity(Activity::Charging(percent), NEWS);
            }
        } else if setting == GUID_BATTERY_PERCENTAGE_REMAINING {
            let before = a.battery.replace(value);
            let low = LOW.into_iter().find(|&t| value <= t && before.is_some_and(|b| b > t));
            if a.on_ac == Some(false) && low.is_some() {
                self.show_activity(Activity::LowBattery(value), NEWS);
            }
        }
    }

    /// Touche des majuscules : enfoncée (sans la répétition), elle bascule le verrouillage.
    fn on_raw_input(&mut self, lp: LPARAM) {
        let mut input = RAWINPUT::default();
        let mut size = size_of::<RAWINPUT>() as u32;
        let read = unsafe { GetRawInputData(HRAWINPUT(lp.0 as _), RID_INPUT, Some(&mut input as *mut RAWINPUT as _), &mut size, size_of::<RAWINPUTHEADER>() as u32) };
        if read == u32::MAX || input.header.dwType != RIM_TYPEKEYBOARD.0 {
            return;
        }
        let keyboard = unsafe { input.data.keyboard };
        if keyboard.VKey != VK_CAPITAL.0 {
            return;
        }
        let down = keyboard.Flags as u32 & RI_KEY_BREAK == 0;
        let a = &mut self.activity;
        if down && !a.caps_down {
            a.caps = !a.caps;
            let on = a.caps;
            self.show_activity(Activity::Caps(on), BRIEF);
        }
        self.activity.caps_down = down;
    }

    /// Les chats Claude Code ont changé : les pastilles suivent, et un chat qui se met à attendre
    /// une réponse, ou qui finit ce qu'il faisait, s'annonce dans l'encoche.
    fn on_chats(&mut self) {
        let chats = self.activity.claude.chats();
        let before = std::mem::replace(&mut self.activity.chats, chats.clone());
        // Dans l'ordre de la liste : une question passe avant un chat qui vient de finir. À la
        // première lecture, rien de neuf : les questions en attente ne s'annoncent pas toutes.
        let first = before.is_empty();
        let news = chats.iter().filter(|_| !first).find_map(|chat| {
            let was = before.iter().find(|c| c.session == chat.session).map(|c| c.status);
            let news = |waiting| Activity::Claude { name: chat.name.clone(), waiting, session: chat.session.clone() };
            match (was, chat.status) {
                (Some(Status::Waiting), Status::Waiting) => None,
                (_, Status::Waiting) => Some(news(true)),
                (Some(Status::Busy), Status::Idle) => Some(news(false)),
                _ => None,
            }
        });
        match news {
            Some(activity) => self.show_activity(activity, CHAT_NEWS),
            None => self.notch_retarget(),
        }
    }

    /// Le chat Claude Code dont l'encoche parle en ce moment, s'il y en a un.
    pub(super) fn claude_activity_session(&self) -> Option<String> {
        match &self.activity.current {
            Some(Activity::Claude { session, .. }) if self.activity.active => Some(session.clone()),
            _ => None,
        }
    }

    /// Ramène un chat au premier plan : l'appli Claude, ou le terminal où il tourne.
    pub(super) fn show_chat(&mut self, session: &str) {
        let Some(chat) = self.activity.chats.iter().find(|c| c.session == session) else {
            return self.show_claude();
        };
        if chat.place != Place::Terminal || chat.pid == 0 {
            return self.show_claude();
        }
        // Dans un terminal : la fenêtre du premier ancêtre du processus qui en a une.
        let parents = claude::parents();
        let windows: Vec<HWND> = win::top_level_windows().into_iter().filter(|&h| win::is_app_window(h)).collect();
        let mut pid = chat.pid;
        for _ in 0..8 {
            let found: Vec<HWND> = windows.iter().copied().filter(|&h| win::app_pid(h) == pid).collect();
            if !found.is_empty() {
                return self.show_windows(&found);
            }
            match parents.get(&pid) {
                Some(&parent) if parent != 0 && parent != pid => pid = parent,
                _ => break,
            }
        }
        self.show_claude();
    }

    /// Ramène la fenêtre de l'appli Claude au premier plan (sa scène si elle en a une).
    pub(super) fn show_claude(&mut self) {
        let windows: Vec<HWND> = win::top_level_windows()
            .into_iter()
            .filter(|&h| win::is_app_window(h))
            .filter(|&h| {
                // L'appli Claude, pas Claude Code (qui s'appelle aussi claude.exe, sans fenêtre).
                win::process_path(win::app_pid(h)).is_some_and(|p| {
                    let p = p.to_lowercase();
                    p.ends_with("\\claude.exe") && !p.contains("claude-code")
                })
            })
            .collect();
        self.show_windows(&windows);
    }

    pub(super) fn show_activity(&mut self, activity: Activity, ms: u32) {
        self.activity.current = Some(activity);
        self.activity.active = true;
        unsafe {
            SetTimer(self.hwnd, TIMER_ACTIVITY, ms, None);
        }
        self.notch_retarget();
    }
}

// --- Dessin ---------------------------------------------------------------------------------

/// Largeur des marques de l'encoche fermée, espaces compris.
pub(super) fn marks_width(marks: &[Mark], k: f32) -> i32 {
    marks.iter().map(|m| mark_width(*m, k) + (6.0 * k).round() as i32).sum::<i32>() - if marks.is_empty() { 0 } else { (6.0 * k).round() as i32 }
}

fn mark_width(mark: Mark, k: f32) -> i32 {
    match mark {
        Mark::Caps | Mark::Claude => (12.0 * k).round() as i32,
        Mark::Camera | Mark::Microphone => (8.0 * k).round() as i32,
        Mark::Chat(_) => (7.0 * k).round() as i32,
    }
}

/// Couleur de la pastille d'un chat.
pub(super) fn chat_color(status: Status) -> (u8, u8, u8) {
    match status {
        Status::Busy => BLUE,
        Status::Waiting => ORANGE,
        Status::Idle => GREEN,
    }
}

/// Logo de Claude : une étoile de rayons fins, autour de (`cx`, `cy`).
fn draw_claude(canvas: &mut Canvas, (cx, cy): (f32, f32), radius: f32, k: f32, alpha: u8) {
    const RAYS: usize = 10;
    for i in 0..RAYS {
        let a = i as f32 * 2.0 * PI / RAYS as f32 - PI / 2.0;
        let (c, s) = (a.cos(), a.sin());
        // Un rayon sur deux un peu plus court, comme sur le logo.
        let length = if i % 2 == 0 { radius } else { radius * 0.78 };
        line(canvas, (cx + c * radius * 0.18, cy + s * radius * 0.18), (cx + c * length, cy + s * length), 0.75 * k, CLAUDE, alpha);
    }
}

/// Dessine les marques, calées à droite sur `right`, centrées sur `cy`.
pub(super) fn draw_marks(canvas: &mut Canvas, marks: &[Mark], right: i32, cy: f32, k: f32, alpha: u8) {
    let gap = (6.0 * k).round() as i32;
    let mut x = right - marks_width(marks, k);
    for &mark in marks {
        let w = mark_width(mark, k);
        let cx = x as f32 + w as f32 / 2.0;
        match mark {
            Mark::Caps => draw_caps(canvas, (cx, cy), 0.62 * k, (235, 235, 240), alpha),
            Mark::Camera | Mark::Microphone => {
                let d = (8.0 * k).round() as i32;
                let top = (cy - d as f32 / 2.0).round() as i32;
                let color = if mark == Mark::Camera { GREEN } else { ORANGE };
                canvas.round_rect(RECT { left: x, top, right: x + d, bottom: top + d }, d / 2, color, alpha);
            }
            Mark::Claude => draw_claude(canvas, (cx, cy), 5.5 * k, k, alpha),
            Mark::Chat(status) => {
                let d = (7.0 * k).round() as i32;
                let top = (cy - d as f32 / 2.0).round() as i32;
                canvas.round_rect(RECT { left: x, top, right: x + d, bottom: top + d }, d / 2, chat_color(status), alpha);
            }
        }
        x += w + gap;
    }
}

/// L'activité, dans la bande `row` sous l'heure : une icône à gauche, puis une jauge ou un
/// texte, et une valeur à droite.
pub(super) fn draw_activity(canvas: &mut Canvas, row: RECT, activity: &Activity, font: HFONT, bold: HFONT, k: f32) {
    let px = |v: f32| (v * k).round() as i32;
    let white = (255, 255, 255);
    let cy = (row.top + row.bottom) as f32 / 2.0;
    let icon = RECT { left: row.left, top: (cy - 10.0 * k).round() as i32, right: row.left + px(20.0), bottom: (cy + 10.0 * k).round() as i32 };
    let text = RECT { left: icon.right + px(10.0), ..row };
    let value = RECT { left: row.right - px(44.0), ..row };
    let mut texts: Vec<(String, RECT, HFONT, COLORREF, DRAW_TEXT_FORMAT)> = Vec::new();
    let line_left = DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX;
    let line_right = DT_RIGHT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX;
    let gauge = |canvas: &mut Canvas, level: f32, dim: bool| {
        let bar = RECT { left: text.left, top: (cy - 2.5 * k).round() as i32, right: value.left - px(8.0), bottom: (cy + 2.5 * k).round() as i32 };
        let radius = (bar.bottom - bar.top) / 2;
        canvas.round_rect(bar, radius, white, 60);
        let x = bar.left + ((bar.right - bar.left) as f32 * level.clamp(0.0, 1.0)).round() as i32;
        if x > bar.left {
            canvas.round_rect(RECT { right: x.max(bar.left + 2 * radius), ..bar }, radius, white, if dim { 120 } else { 235 });
        }
    };
    let percent = |v: f32| format!("{} %", (v * 100.0).round());
    match activity {
        Activity::Volume { level, muted } => {
            draw_volume(canvas, icon, *level, *muted);
            gauge(canvas, *level, *muted);
            texts.push((if *muted { "—".to_owned() } else { percent(*level) }, value, bold, TEXT, line_right));
        }
        Activity::Brightness(level) => {
            draw_sun(canvas, icon, k);
            gauge(canvas, *level, false);
            texts.push((percent(*level), value, bold, TEXT, line_right));
        }
        Activity::Charging(p) | Activity::LowBattery(p) => {
            let charging = matches!(activity, Activity::Charging(_));
            let color = if charging { GREEN } else if *p <= LOW[1] { RED } else { ORANGE };
            draw_battery(canvas, icon, *p, color, charging, k);
            let label = if charging { "En charge" } else { "Batterie faible" };
            texts.push((label.to_owned(), text, font, TEXT, line_left));
            texts.push((format!("{p} %"), value, bold, rgb(color.0, color.1, color.2), line_right));
        }
        Activity::Bluetooth(name) => {
            canvas.round_rect(icon, (icon.right - icon.left) / 2, BLUE, 255);
            draw_bluetooth(canvas, inflate(icon, -px(4.0)));
            let name_rect = RECT { right: row.right - px(70.0), ..text };
            texts.push((name.clone(), name_rect, font, TEXT, line_left));
            texts.push(("Connecté".to_owned(), RECT { left: row.right - px(68.0), ..row }, font, TEXT_DIM, line_right));
        }
        Activity::Caps(on) => {
            draw_caps(canvas, (cx_of(icon), cy), k, white, 255);
            let label = if *on { "Majuscules verrouillées" } else { "Majuscules déverrouillées" };
            texts.push((label.to_owned(), text, font, if *on { TEXT } else { TEXT_DIM }, line_left));
        }
        Activity::Sensor { camera, app } => {
            let d = px(10.0);
            let dot = RECT { left: icon.left + px(5.0), top: (cy - d as f32 / 2.0).round() as i32, right: icon.left + px(5.0) + d, bottom: (cy + d as f32 / 2.0).round() as i32 };
            canvas.round_rect(dot, d / 2, if *camera { GREEN } else { ORANGE }, 255);
            let label = if *camera { "Caméra" } else { "Micro" };
            texts.push((label.to_owned(), RECT { right: text.left + px(70.0), ..text }, font, TEXT, line_left));
            texts.push((app.clone(), RECT { left: text.left + px(70.0), ..row }, font, TEXT_DIM, line_right | DT_END_ELLIPSIS));
        }
        Activity::Claude { name, waiting, .. } => {
            draw_claude(canvas, (cx_of(icon), cy), 8.5 * k, 1.3 * k, 255);
            let (label, color) = if *waiting { ("Question", ORANGE) } else { ("Terminé", GREEN) };
            let name_rect = RECT { right: row.right - px(80.0), ..text };
            texts.push((name.clone(), name_rect, font, TEXT, line_left));
            texts.push((label.to_owned(), RECT { left: row.right - px(78.0), ..row }, bold, rgb(color.0, color.1, color.2), line_right));
        }
        Activity::Timer { title, detail, color } => {
            draw_ring(canvas, (cx_of(icon), cy), 7.5 * k, 1.0, *color, 255, k);
            texts.push((title.clone(), RECT { right: value.left - px(4.0), ..text }, font, TEXT, line_left));
            texts.push((detail.clone(), value, bold, rgb(color.0, color.1, color.2), line_right));
        }
    }
    canvas.gdi(|dc| unsafe {
        SetBkMode(dc, TRANSPARENT);
        for (t, rect, f, color, flags) in &texts {
            SelectObject(dc, *f);
            draw_text(dc, t, *rect, *flags, *color, 0);
        }
    });
}

fn cx_of(r: RECT) -> f32 {
    (r.left + r.right) as f32 / 2.0
}

/// Soleil : un disque et huit rayons.
pub(super) fn draw_sun(canvas: &mut Canvas, r: RECT, k: f32) {
    let (cx, cy) = (cx_of(r), (r.top + r.bottom) as f32 / 2.0);
    let white = (255, 255, 255);
    let d = 4.0 * k;
    let disc = RECT { left: (cx - d).round() as i32, top: (cy - d).round() as i32, right: (cx + d).round() as i32, bottom: (cy + d).round() as i32 };
    canvas.round_rect(disc, d.round() as i32, white, 255);
    for i in 0..8 {
        let a = i as f32 * PI / 4.0;
        let (c, s) = (a.cos(), a.sin());
        line(canvas, (cx + c * 6.5 * k, cy + s * 6.5 * k), (cx + c * 8.8 * k, cy + s * 8.8 * k), 0.8 * k, white, 255);
    }
}

/// Batterie couchée, remplie selon sa charge ; un éclair quand elle charge.
pub(super) fn draw_battery(canvas: &mut Canvas, r: RECT, percent: u32, color: (u8, u8, u8), charging: bool, k: f32) {
    let cy = (r.top + r.bottom) as f32 / 2.0;
    let body = RECT { left: r.left, top: (cy - 5.5 * k).round() as i32, right: r.right - (2.5 * k).round() as i32, bottom: (cy + 5.5 * k).round() as i32 };
    let radius = (2.5 * k).round() as i32;
    canvas.round_rect(body, radius, (150, 150, 158), 255);
    canvas.round_rect(inflate(body, -(1.2 * k).round() as i32), radius - 1, (0, 0, 0), 255);
    let inner = inflate(body, -(2.2 * k).round() as i32);
    let fill = inner.left + ((inner.right - inner.left) as f32 * (percent.min(100) as f32 / 100.0)).round() as i32;
    canvas.round_rect(RECT { right: fill.max(inner.left + 2), ..inner }, (1.2 * k).round() as i32, color, 255);
    let nub = RECT { left: body.right + (0.8 * k).round() as i32, top: (cy - 2.0 * k).round() as i32, right: r.right, bottom: (cy + 2.0 * k).round() as i32 };
    canvas.round_rect(nub, (1.0 * k).round() as i32, (150, 150, 158), 255);
    if charging {
        let cx = cx_of(body);
        let p = |x: f32, y: f32| (cx + x * k, cy + y * k);
        canvas.fill_convex(&[p(1.2, -5.0), p(-2.8, 0.8), p(0.2, 0.8)], (255, 255, 255), 255);
        canvas.fill_convex(&[p(-0.2, -0.8), p(2.8, -0.8), p(-1.2, 5.0)], (255, 255, 255), 255);
    }
}

/// Verrouillage des majuscules : une flèche creuse vers le haut, sur un trait.
fn draw_caps(canvas: &mut Canvas, (cx, cy): (f32, f32), k: f32, color: (u8, u8, u8), alpha: u8) {
    let p = |x: f32, y: f32| (cx + x * k, cy + y * k);
    let head = [p(0.0, -8.0), p(7.0, -1.0), p(-7.0, -1.0)];
    canvas.fill_convex(&round_corners(&head, 1.2 * k), color, alpha);
    let stem = RECT { left: (cx - 3.0 * k).round() as i32, top: (cy - 1.5 * k).round() as i32, right: (cx + 3.0 * k).round() as i32, bottom: (cy + 3.5 * k).round() as i32 };
    canvas.round_rect(stem, (0.8 * k).round() as i32, color, alpha);
    let bar = RECT { left: stem.left, top: (cy + 5.5 * k).round() as i32, right: stem.right, bottom: (cy + 8.0 * k).round() as i32 };
    canvas.round_rect(bar, (0.8 * k).round() as i32, color, alpha);
}
