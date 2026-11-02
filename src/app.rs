//! Barres latérales (une par écran), icône de notification et branchement sur les événements
//! Windows.

use std::{
    cell::RefCell,
    rc::Rc,
    collections::{HashMap, HashSet},
    mem::size_of,
    sync::{
        atomic::{AtomicIsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use windows::{
    core::*,
    Win32::{
        Foundation::*,
        Graphics::{Dwm::*, Gdi::*},
        System::LibraryLoader::GetModuleHandleW,
        UI::{Accessibility::*, HiDpi::*, Input::KeyboardAndMouse::*, Shell::*, WindowsAndMessaging::*},
    },
};

use crate::{
    anim::{self, fit, inflate, offset, size, Flight, Switch},
    autostart,
    canvas::{Canvas, Image, Material},
    media::{Media, NowPlaying},
    settings::{self, Feature, Features},
    shell, taskbar, wallpaper,
    stages::{Model, ScreenId, Transition, Wid},
    win::{self, hwnd_of, wid_of},
};

mod corners;
mod dockbar;
mod float;
mod launcherui;
mod notch;
mod pin;
mod screens;
mod settingsui;

use corners::Corners;
use float::Float;
use dockbar::{DockUi, DOCK, TIMER_CLOCK, WM_DOCKBAR, WM_DOCK_LOADED, WM_DOCK_WALLPAPER};
use launcherui::{LauncherUi, HOTKEY_LAUNCHER, LAUNCHER, WM_LAUNCHER_APPS, WM_LAUNCHER_ICON};
use notch::NotchUi;
use pin::{Pin, HOTKEY_PIN};
use screens::{Screen, BAR_CLASS, NOTCH_CLASS};
use settingsui::{SettingsUi, SETTINGS, WM_SHOW_SETTINGS};

const WM_WINEVENT: u32 = WM_APP + 1;
const WM_TRAY: u32 = WM_APP + 2;
const WM_APPBAR: u32 = WM_APP + 3;
const WM_MEDIA: u32 = WM_APP + 4;
/// Envoyé avec WM_SETTINGCHANGE quand le fond d'écran change.
const SPI_SETDESKWALLPAPER: usize = 0x0014;
const WM_MOUSELEAVE: u32 = 0x02A3;
const TIMER_RESCAN: usize = 1;
const TIMER_WINDOW_DRAG: usize = 2;
/// Les écrans ont changé : on attend que la rafale de messages soit passée.
const TIMER_SCREENS: usize = 9;
const HOTKEY_TOGGLE: i32 = 1;
const HOTKEY_DETACH: i32 = 2;
const MENU_SETTINGS: usize = 1;
const MENU_QUIT: usize = 2;
const MENU_SHOW: usize = 3;
const MENU_MERGE: usize = 4;
const MENU_AUTOSTART: usize = 5;
const MENU_SPLIT: usize = 6;

/// Barre des scènes : une petite pastille noire d'icônes collée en haut de l'écran, à gauche de
/// l'encoche. Mesures en pixels à 100 %.
const BAR_H: i32 = 32;
const ICON: i32 = 20;
const ICON_GAP: i32 = 10;
const PILL_PAD: i32 = 12;
const PILL_RADIUS: i32 = 10;
/// Congés creux qui raccordent la pastille au bord de l'écran, comme l'encoche.
const PILL_SHOULDER: i32 = 6;
/// Écart entre la pastille et l'encoche (congés compris).
const NOTCH_GAP: i32 = 18;
/// Nombre d'icônes au plus.
const MAX_ICONS: i32 = 8;
/// Taille de la fenêtre de la barre : la pastille la plus large, et un peu de marge.
const BAR_WIN_W: i32 = MAX_ICONS * (ICON + ICON_GAP) - ICON_GAP + 2 * (PILL_PAD + PILL_SHOULDER) + 4;
const BAR_WIN_H: i32 = BAR_H + 8;
/// Grossissement de l'icône survolée, et durée de son ressort.
const HOVER_SCALE: f64 = 0.35;
const HOVER_ANIM: Duration = Duration::from_millis(280);
/// La pastille rentre dans le bord de l'écran quand l'encoche s'ouvre par-dessus.
const VEIL_ANIM: Duration = Duration::from_millis(260);
/// Après un changement de scène, on ignore les événements parasites qu'il provoque.
const SWITCH_GUARD: Duration = Duration::from_millis(500);
/// Durée par défaut des animations (modifiable avec STAGE_MANAGER_ANIM_MS, 0 pour les couper).
const ANIM_MS: u64 = 650;
/// Une fois les vraies fenêtres revenues sous les miniatures animées : le temps qu'elles se
/// redessinent, puis le fondu qui efface les miniatures par-dessus. Pas de coupure nette : une
/// fenêtre qui n'a pas fini de se dessiner apparaît en douceur au lieu de clignoter.
const SETTLE: Duration = Duration::from_millis(90);
const SETTLE_FADE: Duration = Duration::from_millis(140);
/// Au-delà, on n'attend plus une fenêtre qui tarde à revenir.
const SETTLE_MAX: Duration = Duration::from_millis(600);

const TEXT: COLORREF = rgb(245, 245, 248);
const TEXT_DIM: COLORREF = rgb(200, 200, 208);

/// Fenêtre principale, invisible : elle reçoit les événements, les minuteurs et les raccourcis.
static MAIN: AtomicIsize = AtomicIsize::new(0);
/// Fenêtres réduites par nous, à restaurer quoi qu'il arrive en quittant.
static HIDDEN: Mutex<Vec<Wid>> = Mutex::new(Vec::new());

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16)
}

/// Remet tout en état (sortie, panique) : les fenêtres qu'on a réduites, la page accrochée,
/// la barre des tâches.
pub fn restore_hidden() {
    restore_minimized();
    // La page accrochée à l'encoche, si elle était garée hors de l'écran.
    pin::restore_parked();
    // La fenêtre épinglée, si elle était réduite en vignette, et son « toujours au premier plan ».
    float::restore();
    // Et la barre des tâches, si on l'avait passée en masquage automatique pour le Dock.
    taskbar::restore();
}

/// Restaure les fenêtres réduites par les scènes.
fn restore_minimized() {
    let windows = std::mem::take(&mut *HIDDEN.lock().unwrap_or_else(|e| e.into_inner()));
    for w in windows {
        let h = hwnd_of(w);
        if unsafe { IsWindow(h).as_bool() } && win::is_minimized(h) {
            win::show(h, SW_SHOWNOACTIVATE);
        }
    }
}

/// Lancé une deuxième fois : on ouvre les réglages de celui qui tourne déjà.
pub fn show_running_settings() {
    unsafe {
        let Ok(main) = FindWindowW(w!("StageManagerSidebar"), None) else { return };
        let mut pid = 0;
        GetWindowThreadProcessId(main, Some(&mut pid));
        // Pour qu'il puisse passer sa fenêtre au premier plan.
        let _ = AllowSetForegroundWindow(pid);
        let _ = PostMessageW(main, WM_SHOW_SETTINGS, WPARAM(0), LPARAM(0));
    }
}

/// Les écrans ont changé (branchement, résolution, mise à l'échelle) : on les relira bientôt.
fn screens_changed() {
    unsafe {
        SetTimer(HWND(MAIN.load(Ordering::Relaxed) as _), TIMER_SCREENS, 300, None);
    }
}

#[derive(Clone)]
struct Card {
    stage: u64,
    window: Wid,
    /// Zone cliquable autour de l'icône (coordonnées de la barre).
    rect: RECT,
    /// L'icône elle-même : les fenêtres de la scène partent d'ici et y reviennent.
    thumb_box: RECT,
    dest: RECT,
    icon: Rc<Image>,
    /// Écart avec la position précédente, résorbé par l'animation de glissement.
    slide: (i32, i32),
    /// Carte qu'on est en train de déplacer à la souris : elle quitte la barre.
    hidden: bool,
    /// Carte qui vient d'apparaître dans la barre : elle arrive en fondu.
    fade_in: bool,
}

type LayoutKey = Vec<(u64, Wid, usize, RECT)>;

/// Carte attrapée à la souris.
struct Drag {
    stage: u64,
    window: Wid,
    /// Point où le bouton a été enfoncé (coordonnées de la barre).
    start: POINT,
    /// Miniature de la carte au moment où on l'attrape (coordonnées de l'overlay).
    origin: RECT,
    /// Miniature qui suit la souris, une fois le glisser commencé.
    ghost: Option<isize>,
    rect: RECT,
}

struct App {
    /// Fenêtre principale, invisible (voir `MAIN`).
    hwnd: HWND,
    /// Les scènes de tous les écrans : chacune appartient à un écran.
    model: Model,
    /// Fonctionnalités en service (les scènes, le Dock…), d'après les réglages.
    features: Features,
    /// Fenêtres qu'on vient de réduire : leur événement de réduction vient de nous.
    pending_minimize: HashSet<Wid>,
    /// Position de chaque fenêtre au moment où on l'a réduite : c'est là qu'elle reviendra.
    /// (Pour une fenêtre ancrée sur un bord, Windows ne donne que sa position d'avant l'ancrage.)
    last_rects: HashMap<Wid, RECT>,
    /// Peut-on piloter les fenêtres de ce processus ? (pas s'il est admin et nous non)
    manageable: HashMap<u32, bool>,
    self_elevated: bool,
    /// Icône de chaque fenêtre de la barre, et celle de chaque appli (par chemin).
    icons: HashMap<Wid, Rc<Image>>,
    app_icons: HashMap<String, Rc<Image>>,
    media: Media,
    /// Morceau en cours, affiché dans les encoches.
    now_playing: Option<NowPlaying>,
    /// Les écrans, le principal en premier : chacun a sa barre des scènes et son encoche.
    screens: Vec<Screen>,
    /// Dernier identifiant d'écran attribué.
    next_screen: ScreenId,
    /// L'écran principal (le Dock, le lanceur, les réglages et les coins y sont).
    monitor: RECT,
    taskbar_created: u32,
    anim_duration: Duration,
    /// Le Dock, en bas de l'écran principal.
    dock: DockUi,
    /// Le lanceur façon Spotlight (Alt + Espace).
    launcher: LauncherUi,
    /// Page accrochée aux encoches, qu'elles montrent en direct.
    pin: Pin,
    /// Fenêtre épinglée au premier plan, déposée sur une encoche.
    float: Float,
    /// Coins arrondis de l'écran.
    corners: Corners,
    /// La fenêtre des réglages.
    settings: SettingsUi,
    /// Fenêtre d'une scène active qu'on déplace par sa barre de titre : la lâcher sur la barre
    /// de son écran la sort de sa scène.
    window_drag: Option<WindowDrag>,
    /// Écran dont la souris survole la barre pendant ce déplacement : cette barre s'éclaire.
    drop_target: Option<ScreenId>,
}

/// Fenêtre déplacée par l'utilisateur, avec sa position d'avant le déplacement.
#[derive(Clone, Copy)]
struct WindowDrag {
    window: Wid,
    rect: RECT,
    maximized: bool,
}

impl App {
    fn new(hwnd: HWND, dock: HWND, launcher: HWND, settings: HWND, corners: Corners) -> Self {
        let anim_ms = std::env::var("STAGE_MANAGER_ANIM_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(ANIM_MS);
        Self {
            hwnd,
            model: Model::default(),
            features: Features::NONE,
            pending_minimize: HashSet::new(),
            last_rects: HashMap::new(),
            manageable: HashMap::new(),
            self_elevated: win::is_elevated(None).unwrap_or(false),
            icons: HashMap::new(),
            app_icons: HashMap::new(),
            media: Media::start(hwnd, WM_MEDIA),
            now_playing: None,
            screens: Vec::new(),
            next_screen: 0,
            monitor: RECT::default(),
            taskbar_created: unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) },
            anim_duration: Duration::from_millis(anim_ms),
            dock: DockUi::new(dock),
            launcher: LauncherUi::new(launcher),
            pin: Pin::new(),
            float: Float::new(),
            corners,
            settings: SettingsUi::new(settings),
            window_drag: None,
            drop_target: None,
        }
    }

    fn handle(&mut self, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
        if self.handle_dock_message(msg, wp, lp)
            || self.handle_launcher_message(msg, wp)
            || self.handle_pin_message(msg, wp)
            || self.handle_float_message(msg, wp)
            || self.handle_settings_message(msg, wp)
        {
            return Some(LRESULT(0));
        }
        match msg {
            WM_WINEVENT => self.on_event(wp.0 as u32, HWND(lp.0 as _)),
            WM_MEDIA => {
                // Le lecteur est dans les encoches.
                self.now_playing = self.media.now_playing();
                self.notch_retarget();
            }
            WM_SETTINGCHANGE if wp.0 == SPI_SETDESKWALLPAPER => self.reload_dock_backdrop(),
            WM_TIMER if wp.0 == TIMER_RESCAN => self.rescan(),
            WM_TIMER if wp.0 == TIMER_WINDOW_DRAG => {
                let target = self.window_drop_target();
                self.set_drop_target(target);
            }
            WM_TIMER if wp.0 == TIMER_SCREENS => {
                unsafe {
                    let _ = KillTimer(self.hwnd, TIMER_SCREENS);
                }
                self.update_screens();
            }
            WM_HOTKEY if wp.0 == HOTKEY_TOGGLE as usize => self.set_feature(Feature::Scenes, !self.features.scenes),
            WM_HOTKEY if wp.0 == HOTKEY_DETACH as usize && self.features.scenes => {
                let foreground = wid_of(unsafe { GetForegroundWindow() });
                self.detach(foreground, None);
            }
            WM_DISPLAYCHANGE | WM_DPICHANGED => screens_changed(),
            WM_ENDSESSION if wp.0 != 0 => restore_hidden(),
            WM_CLOSE => {}
            m if m == self.taskbar_created => {
                // L'explorateur a redémarré : il a perdu notre icône et nos barres.
                self.add_tray();
                for s in 0..self.screens.len() {
                    self.register_appbar(s);
                    if self.screens[s].appbar.take().is_some() {
                        self.reserve_bar(s);
                    }
                    if self.screens[s].notch_band.is_some() {
                        self.screens[s].notch_band = Some(RECT::default());
                        self.reserve_notch_band(s);
                    }
                }
                self.reregister_dock();
            }
            _ => return None,
        }
        Some(LRESULT(0))
    }

    /// Message reçu par la barre d'un écran : sa place réservée, le plein écran, la souris.
    fn bar_message(&mut self, hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) {
        let Some(s) = self.bar_index(hwnd) else { return };
        match msg {
            WM_APPBAR => match wp.0 as u32 {
                ABN_POSCHANGED => {
                    self.place_appbar(s);
                    self.reserve_notch_band(s);
                }
                // Reçu même quand la barre ne réserve aucune place (voir `register_appbar`).
                ABN_FULLSCREENAPP => self.on_fullscreen(lp.0 != 0),
                _ => {}
            },
            WM_MOUSEMOVE if self.screens[s].drag.is_some() => self.drag_to(s, point(lp)),
            WM_MOUSEMOVE => self.on_mouse_move(s, point(lp)),
            WM_MOUSELEAVE => {
                self.screens[s].tracking_mouse = false;
                self.set_hover(s, None);
            }
            WM_LBUTTONDOWN => self.press(s, point(lp)),
            WM_LBUTTONUP => self.release(s, point(lp)),
            // Capture perdue (Alt+Tab, Échap…) : on annule le glisser.
            WM_CAPTURECHANGED => self.cancel_drag(s),
            _ => {}
        }
    }

    // --- Activation ---------------------------------------------------------------

    /// Démarrage : les écrans (et la surveillance du plein écran), puis les fonctionnalités
    /// choisies.
    fn start(&mut self) {
        self.update_screens();
        self.apply_features(settings::load());
    }

    /// Arrêt : tout est remis en état, sans toucher aux réglages enregistrés.
    fn stop(&mut self) {
        self.close_settings();
        // La page accrochée reste ouverte, ramenée à l'écran.
        if self.features.active(Feature::Pin) {
            self.disable_pin(false);
        }
        self.features.pin = false;
        // La fenêtre épinglée reprend sa place ; la barre d'ancrage de Windows reste coupée.
        if self.features.active(Feature::Float) {
            self.disable_float(false);
        }
        self.features.float = false;
        self.apply_features(Features::NONE);
        for s in 0..self.screens.len() {
            self.unregister_appbar(s);
        }
    }

    /// Active ou désactive une fonctionnalité, et l'enregistre.
    pub(super) fn set_feature(&mut self, f: Feature, on: bool) {
        let mut wanted = self.features;
        wanted.set(f, on);
        self.apply_features(wanted);
        settings::save(&self.features);
    }

    fn set_hotkey(&self, id: i32, on: bool, modifiers: HOT_KEY_MODIFIERS, key: u32) {
        unsafe {
            if on {
                let _ = RegisterHotKey(self.hwnd, id, modifiers | MOD_NOREPEAT, key);
            } else {
                let _ = UnregisterHotKey(self.hwnd, id);
            }
        }
    }

    /// Met en route ou arrête ce qu'il faut pour passer aux fonctionnalités `wanted`.
    fn apply_features(&mut self, wanted: Features) {
        let old = self.features;
        if old == wanted {
            return;
        }
        self.features = wanted;
        let off = |f: Feature| old.active(f) && !wanted.active(f);
        let on = |f: Feature| !old.active(f) && wanted.active(f);
        // D'abord les arrêts (la page avant l'encoche qui la montre)…
        if off(Feature::Pin) {
            self.set_hotkey(HOTKEY_PIN, false, MOD_CONTROL | MOD_ALT, 'P' as u32);
            self.disable_pin(true);
        }
        if off(Feature::Float) {
            self.disable_float(true);
        }
        if off(Feature::Scenes) {
            self.disable_scenes();
        }
        if off(Feature::Dock) {
            self.disable_dock();
        }
        if off(Feature::Notch) {
            self.disable_notch();
        }
        if off(Feature::Corners) {
            self.corners.hide();
        }
        if off(Feature::Launcher) {
            self.set_hotkey(HOTKEY_LAUNCHER, false, MOD_ALT, VK_SPACE.0 as u32);
            self.close_launcher(true);
        }
        // … puis les mises en route.
        if on(Feature::Dock) {
            self.enable_dock();
        }
        if on(Feature::Notch) {
            self.enable_notch();
        }
        if on(Feature::Corners) {
            self.corners.show(self.monitor);
        }
        if on(Feature::Launcher) {
            self.set_hotkey(HOTKEY_LAUNCHER, true, MOD_ALT, VK_SPACE.0 as u32);
            self.preload_launcher();
        }
        if on(Feature::Scenes) {
            self.enable_scenes();
        }
        if on(Feature::Pin) {
            self.set_hotkey(HOTKEY_PIN, true, MOD_CONTROL | MOD_ALT, 'P' as u32);
            self.enable_pin();
        }
        if on(Feature::Float) {
            self.enable_float();
        }
        // L'encoche change de forme avec la page accrochée.
        if off(Feature::Pin) || on(Feature::Pin) {
            self.notch_retarget();
        }
        unsafe {
            // L'horloge sert au Dock (volume) et aux encoches (heure) ; la surveillance des
            // fenêtres, aux scènes et aux applis ouvertes du Dock.
            if wanted.dock || wanted.notch {
                SetTimer(self.hwnd, TIMER_CLOCK, 1000, None);
            } else {
                let _ = KillTimer(self.hwnd, TIMER_CLOCK);
            }
            if wanted.scenes || wanted.dock {
                SetTimer(self.hwnd, TIMER_RESCAN, 1500, None);
            } else {
                let _ = KillTimer(self.hwnd, TIMER_RESCAN);
            }
        }
        self.render_settings();
    }

    /// Une barre par écran, et une scène par application sur chaque écran.
    fn enable_scenes(&mut self) {
        for s in 0..self.screens.len() {
            self.show_bar(s);
        }
        let mut windows: Vec<(Wid, String, ScreenId)> = Vec::new();
        for h in win::top_level_windows() {
            if win::is_minimized(h) {
                continue;
            }
            let (Some(app), Some(screen)) = (self.app_key(h), self.screen_at(h)) else { continue };
            windows.push((wid_of(h), app, screen));
        }
        let foreground = unsafe { GetForegroundWindow() };
        let t = self.model.init(&windows, Some(wid_of(foreground)));
        self.apply_now(t);
        self.refresh_all();
    }

    /// Toutes les fenêtres sont restaurées, les barres rendent le bord des écrans.
    fn disable_scenes(&mut self) {
        unsafe {
            let _ = KillTimer(self.hwnd, TIMER_WINDOW_DRAG);
        }
        self.window_drag = None;
        self.drop_target = None;
        for s in 0..self.screens.len() {
            self.cancel_drag(s);
            // Termine l'animation (et replace les fenêtres détachées) avant de tout restaurer.
            self.finish_switch(s);
            self.screens[s].slide_start = None;
        }
        restore_minimized();
        self.model = Model::default();
        self.pending_minimize.clear();
        self.icons.clear();
        self.last_rects.clear();
        for s in 0..self.screens.len() {
            let sc = &mut self.screens[s];
            sc.cards.clear();
            sc.departing = None;
            sc.layout_key.clear();
            sc.hover = None;
            sc.pill_from = 0;
            sc.pill_to = 0;
            unsafe {
                let _ = ShowWindow(sc.hwnd, SW_HIDE);
            }
            self.release_bar(s);
        }
    }

    /// Réserve le bord de l'écran et montre sa barre (sauf devant une appli plein écran).
    fn show_bar(&mut self, s: usize) {
        self.reserve_bar(s);
        if !self.screens[s].fullscreen {
            unsafe {
                let _ = ShowWindow(self.screens[s].hwnd, SW_SHOWNOACTIVATE);
            }
        }
    }

    // --- Scènes ---------------------------------------------------------------------

    /// Clé d'application d'une fenêtre, ou `None` si on ne doit pas la gérer.
    fn app_key(&mut self, h: HWND) -> Option<String> {
        // La page accrochée à l'encoche vit à part, garée hors de l'écran.
        if !win::is_app_window(h) || self.is_pin_window(h) {
            return None;
        }
        let owner = win::pid(h);
        let self_elevated = self.self_elevated;
        // Sans droits admin, Windows nous interdit de piloter les fenêtres admin.
        let manageable = *self
            .manageable
            .entry(owner)
            .or_insert_with(|| self_elevated || win::is_elevated(Some(owner)) == Some(false));
        if !manageable {
            return None;
        }
        let pid = win::app_pid(h);
        Some(win::process_path(pid).unwrap_or_else(|| format!("pid:{pid}")))
    }

    fn on_event(&mut self, event: u32, h: HWND) {
        if event == EVENT_SYSTEM_FOREGROUND {
            // Une fenêtre « toujours au premier plan » a pu passer devant les coins.
            self.corners.raise();
        }
        if self.float_event(event, h) || self.pin_event(event, h) {
            return;
        }
        if !self.features.scenes {
            // Sans les scènes, le Dock suit quand même les applis qu'on ouvre.
            if event == EVENT_SYSTEM_FOREGROUND {
                self.update_running();
            }
            return;
        }
        let w = wid_of(h);
        match event {
            EVENT_SYSTEM_FOREGROUND => {
                let Some(here) = self.screen_at(h) else { return };
                let screen = self.model.screen_of(w).unwrap_or(here);
                if self.guarded(screen) && !self.model.in_active_stage(w) {
                    return;
                }
                // Passée sur un autre écran sans qu'on le voie : elle rejoint ses scènes.
                if self.model.is_tracked(w) && !win::is_minimized(h) {
                    self.follow_screen(w, here);
                }
                let t = if let Some(id) = self.model.stage_of(w) {
                    self.model.activate(id, Some(w))
                } else if win::is_minimized(h) {
                    return;
                } else {
                    let Some(app) = self.app_key(h) else { return };
                    self.model.add_focused(w, &app, here)
                };
                let screen = self.model.screen_of(w).unwrap_or(here);
                self.apply(screen, t, true);
            }
            EVENT_SYSTEM_MINIMIZESTART => {
                if self.pending_minimize.remove(&w) || !self.model.in_active_stage(w) {
                    return;
                }
                // Réduite par l'utilisateur : elle rejoint la barre, d'où un clic la ramène. Si
                // c'était la dernière de la scène active, l'écran reste dégagé.
                self.model.shelve(w);
            }
            EVENT_SYSTEM_MINIMIZEEND => {
                let Some(screen) = self.model.screen_of(w) else { return };
                if self.guarded(screen) || self.model.in_active_stage(w) {
                    return;
                }
                let Some(id) = self.model.stage_of(w) else { return };
                let t = self.model.activate(id, Some(w));
                self.apply(screen, t, true);
            }
            EVENT_OBJECT_HIDE | EVENT_OBJECT_DESTROY => {
                let Some(screen) = self.model.screen_of(w) else { return };
                if win::is_alive(h) {
                    return;
                }
                // Seule la fermeture de la scène active fait venir la suivante : un écran
                // dégagé exprès (tout réduit) le reste.
                let was_active = self.model.in_active_stage(w);
                self.forget(w);
                if was_active {
                    self.show_next_stage(screen);
                }
            }
            EVENT_SYSTEM_MOVESIZESTART => {
                if !self.model.in_active_stage(w) {
                    return;
                }
                let maximized = unsafe { IsZoomed(h) }.as_bool();
                self.window_drag = Some(WindowDrag { window: w, rect: anim::window_rect(h), maximized });
                // Pendant le déplacement, Windows ne prévient pas : on surveille la souris.
                unsafe {
                    SetTimer(self.hwnd, TIMER_WINDOW_DRAG, 30, None);
                }
                return;
            }
            EVENT_SYSTEM_MOVESIZEEND => {
                if self.window_drag.is_none_or(|d| d.window != w) {
                    return;
                }
                let target = self.window_drop_target();
                let Some(drag) = self.window_drag.take() else { return };
                unsafe {
                    let _ = KillTimer(self.hwnd, TIMER_WINDOW_DRAG);
                }
                self.set_drop_target(None);
                if target.is_some() {
                    self.detach(w, Some(drag));
                } else if let Some(here) = self.screen_at(h) {
                    // Lâchée sur un autre écran : elle rejoint sa scène active.
                    self.follow_screen(w, here);
                }
            }
            _ => return,
        }
        self.refresh_all();
        // Une appli a pu s'ouvrir ou se fermer : le Dock suit.
        self.update_running();
    }

    /// Une fenêtre affichée est passée sur l'écran `here` : elle rejoint sa scène active. Si
    /// la scène qu'elle quitte s'est vidée, la suivante de la barre de son ancien écran la
    /// remplace.
    fn follow_screen(&mut self, w: Wid, here: ScreenId) {
        if let Some(from) = self.model.move_window(w, here) {
            self.show_next_stage(from);
        }
    }

    /// Sort une fenêtre de la scène active de son écran : elle rejoint la barre comme scène à
    /// part. `moved` : si on l'a traînée jusqu'à la barre, sa position d'avant le déplacement,
    /// pour qu'elle revienne là et non à l'endroit où on l'a lâchée.
    fn detach(&mut self, w: Wid, moved: Option<WindowDrag>) {
        let Some(screen) = self.model.screen_of(w) else { return };
        let t = self.model.detach(w);
        if t.is_empty() {
            return;
        }
        self.apply(screen, t, true);
        if let (Some(drag), Some(s)) = (moved, self.screen_index(screen)) {
            self.last_rects.insert(w, drag.rect);
            self.screens[s].pending_placements.push(drag);
            if self.screens[s].switch.is_none() {
                self.apply_placements(s);
            }
        }
        self.refresh_all();
    }

    /// À appeler une fois les fenêtres réduites : changer leur position ne se voit plus.
    fn apply_placements(&mut self, s: usize) {
        for drag in std::mem::take(&mut self.screens[s].pending_placements) {
            anim::set_restore_position(hwnd_of(drag.window), drag.rect, drag.maximized);
        }
    }

    /// Écran dont la barre est sous la fenêtre en cours de déplacement, si on peut l'y lâcher :
    /// la barre de son propre écran, et il doit rester une autre fenêtre dans sa scène.
    fn window_drop_target(&self) -> Option<ScreenId> {
        let drag = self.window_drag?;
        // Un redimensionnement passe par les mêmes événements : on ne retient que les
        // déplacements (taille inchangée). Une fenêtre agrandie reprend sa taille normale
        // dès qu'on la tire, ce n'est pas un redimensionnement.
        let resized = size(anim::window_rect(hwnd_of(drag.window))) != size(drag.rect);
        // Lâchée sur la zone de dépôt de l'encoche, elle est épinglée, pas sortie de sa scène.
        let pinning = self.screens.iter().any(|sc| matches!(sc.notch.drop, Some(notch::DropZone::Hot { .. } | notch::DropZone::Done { .. })));
        if pinning || (resized && !drag.maximized) || !self.model.can_detach(drag.window) {
            return None;
        }
        let screen = self.model.screen_of(drag.window)?;
        let bar = self.screens[self.screen_index(screen)?].appbar?;
        let mut pt = POINT::default();
        unsafe { GetCursorPos(&mut pt) }.ok()?;
        contains(bar, pt).then_some(screen)
    }

    fn set_drop_target(&mut self, target: Option<ScreenId>) {
        if self.drop_target == target {
            return;
        }
        let old = std::mem::replace(&mut self.drop_target, target);
        for screen in [old, target].into_iter().flatten() {
            if let Some(s) = self.screen_index(screen) {
                self.render(s);
            }
        }
    }

    /// La scène active d'un écran vient de se vider (dernière fenêtre fermée, réduite ou partie
    /// sur un autre écran) : la scène la plus récente de sa barre prend sa place, plutôt que de
    /// laisser le bureau seul à l'écran.
    fn show_next_stage(&mut self, screen: ScreenId) {
        if let Some(next) = self.model.next_stage(screen) {
            let t = self.model.activate(next, None);
            self.apply(screen, t, true);
        }
    }

    fn forget(&mut self, w: Wid) {
        self.model.remove(w);
        self.pending_minimize.remove(&w);
        self.icons.remove(&w);
        self.last_rects.remove(&w);
        HIDDEN.lock().unwrap_or_else(|e| e.into_inner()).retain(|x| *x != w);
    }

    /// Filet de sécurité : purge les fenêtres disparues, adopte celles apparues sans focus et
    /// suit celles passées sur un autre écran sans qu'on le voie (Win + Maj + flèche…).
    fn rescan(&mut self) {
        // Le Dock suit les applis ouvertes, avec ou sans les scènes.
        self.update_running();
        if !self.features.scenes {
            return;
        }
        for w in self.model.windows() {
            if !win::is_alive(hwnd_of(w)) {
                self.forget(w);
            }
        }
        for h in win::top_level_windows() {
            let w = wid_of(h);
            if win::is_minimized(h) {
                continue;
            }
            let Some(here) = self.screen_at(h) else { continue };
            if self.guarded(here) {
                continue;
            }
            match self.model.screen_of(w) {
                Some(from) if from != here && !self.guarded(from) && self.model.in_active_stage(w) => {
                    self.follow_screen(w, here);
                }
                Some(_) => {}
                None => {
                    if let Some(app) = self.app_key(h) {
                        self.model.adopt(w, &app, here);
                    }
                }
            }
        }
        self.refresh_all();
        // Les titres des fenêtres ont pu changer.
        self.render_all();
    }

    fn show_stage(&mut self, stage: u64, merge: bool) {
        let Some(screen) = self.model.stage(stage).map(|s| s.screen) else { return };
        let t = if merge { self.model.merge_into_active(stage) } else { self.model.activate(stage, None) };
        self.apply(screen, t, true);
        self.refresh_all();
    }

    /// Applique un changement de scène sur l'écran `screen`, animé si possible.
    fn apply(&mut self, screen: ScreenId, t: Transition, animate: bool) {
        if t.is_empty() {
            return;
        }
        let Some(s) = self.screen_index(screen) else {
            self.apply_now(t);
            return;
        };
        self.finish_switch(s);
        self.prepare(&t);
        let sc = &self.screens[s];
        let animate = animate && !self.anim_duration.is_zero() && sc.appbar.is_some();
        let extra = if animate { self.anim_duration } else { Duration::ZERO };
        self.screens[s].guard_until = Instant::now() + SWITCH_GUARD + extra;
        if animate && self.start_switch(s, &t) {
            return;
        }
        self.reveal(&t.restore, t.focus);
        self.hide(&t.minimize);
    }

    /// Applique un changement sans animation, sur tous les écrans à la fois (mise en route des
    /// scènes, écran débranché).
    fn apply_now(&mut self, t: Transition) {
        if t.is_empty() {
            return;
        }
        let guard = Instant::now() + SWITCH_GUARD;
        for s in 0..self.screens.len() {
            self.finish_switch(s);
            self.screens[s].guard_until = guard;
        }
        self.prepare(&t);
        self.reveal(&t.restore, t.focus);
        self.hide(&t.minimize);
    }

    /// Avant de réduire des fenêtres : la liste à restaurer en quittant.
    fn prepare(&mut self, t: &Transition) {
        let mut hidden = HIDDEN.lock().unwrap_or_else(|e| e.into_inner());
        hidden.retain(|w| !t.restore.contains(w) && t.focus != Some(*w));
        for &w in &t.minimize {
            if !hidden.contains(&w) {
                hidden.push(w);
            }
        }
    }

    /// Lance l'animation d'un changement de scène sur l'écran `s`. Renvoie `false` s'il n'y a
    /// rien à animer.
    fn start_switch(&mut self, s: usize, t: &Transition) -> bool {
        let now = Instant::now();
        let (mon, bar, overlay) = {
            let sc = &self.screens[s];
            (sc.rect, sc.appbar.unwrap_or_default(), sc.overlay)
        };
        let to_overlay = |r: RECT| offset(r, -mon.left, -mon.top);
        let bar_to_overlay = |r: RECT| offset(r, bar.left - mon.left, bar.top - mon.top);

        // Les fenêtres entrantes partent de leur carte actuelle dans la barre.
        let incoming: Vec<Wid> = t.restore.iter().copied().chain(t.focus).collect();
        // Si on vient de lâcher une carte, elles partent de là où on l'a lâchée.
        let dropped = self.screens[s].drag_source.take();
        let card = self.screens[s].cards.iter().find(|c| incoming.contains(&c.window)).map(|c| {
            // Figée à sa position affichée : elle va s'effacer pendant que ses fenêtres partent.
            let (dx, dy) = self.card_offset(s, c, now);
            Card {
                rect: offset(c.rect, dx, dy),
                thumb_box: offset(c.thumb_box, dx, dy),
                dest: offset(c.dest, dx, dy),
                slide: (0, 0),
                fade_in: false,
                ..c.clone()
            }
        });
        let source = dropped.or_else(|| card.as_ref().map(|c| bar_to_overlay(c.thumb_box)));
        self.screens[s].departing = if dropped.is_none() { card } else { None };

        // La scène sortante prend la première place de la barre ; sa carte reste cachée
        // jusqu'à ce que les miniatures l'aient rejointe.
        self.screens[s].arriving = t.minimize.first().and_then(|w| self.model.stage_of(*w));
        self.refresh(s);
        // Les fenêtres sortantes rapetissent jusqu'à l'icône de leur scène.
        let target = {
            let sc = &self.screens[s];
            sc.arriving
                .and_then(|id| sc.cards.iter().find(|c| c.stage == id))
                .map(|c| bar_to_overlay(c.thumb_box))
                .unwrap_or_else(|| bar_to_overlay(self.icon_slot(s, 0, 1)))
        };

        // Fenêtres concernées, de l'avant vers l'arrière, avec leur position à l'écran.
        let outgoing: Vec<(Wid, RECT)> = t
            .minimize
            .iter()
            .filter(|w| !win::is_minimized(hwnd_of(**w)))
            .map(|&w| (w, to_overlay(anim::window_rect(hwnd_of(w)))))
            .collect();
        let arriving: Vec<(Wid, RECT)> = incoming
            .iter()
            .rev()
            .filter(|w| win::is_minimized(hwnd_of(**w)))
            .map(|&w| {
                let rect = self.last_rects.get(&w).copied().unwrap_or_else(|| anim::restored_rect(hwnd_of(w)));
                (w, to_overlay(rect))
            })
            .collect();
        // Toutes perdent l'animation de Windows, même celles qui ne volent pas.
        let quiet: Vec<Wid> = outgoing.iter().chain(&arriving).map(|(w, _)| *w).collect();

        let mut flights = Vec::new();
        // `window` : la fenêtre à l'écran ; `icon` : là où elle rapetisse (ou d'où elle part).
        let mut fly = |w: Wid, window: RECT, icon: Option<RECT>, fade_in: bool| unsafe {
            if let Ok(thumb) = DwmRegisterThumbnail(overlay, hwnd_of(w)) {
                // La miniature ne montre que la partie visible de la fenêtre, sans ses bordures
                // invisibles : elle doit se poser exactement dessus.
                let real = anim::visible_part(window, DwmQueryThumbnailSourceSize(thumb).unwrap_or_default());
                let small = match icon {
                    Some(icon) => fit(icon, size(real)),
                    None => inflate(real, -(real.right - real.left) / 10),
                };
                let (from, to) = if fade_in { (small, real) } else { (real, small) };
                anim::place_thumb(thumb, from, true, if fade_in { 0 } else { 255 });
                flights.push(Flight { thumb, from, to, fade_in });
            }
        };
        // Seules les fenêtres visibles volent, enregistrées de l'arrière vers l'avant
        // pour que la fenêtre de devant reste dessus.
        for (w, window) in anim::visible_layers(&outgoing).into_iter().rev() {
            fly(w, window, Some(target), false);
        }
        for (w, window) in anim::visible_layers(&arriving).into_iter().rev() {
            fly(w, window, source, true);
        }
        if flights.is_empty() {
            self.screens[s].departing = None;
            self.reveal_arriving(s);
            return false;
        }

        for &w in &quiet {
            anim::set_transitions(hwnd_of(w), false);
        }
        // Les miniatures doivent être à l'écran avant que les vraies fenêtres disparaissent.
        // Si la fenêtre à activer est déjà là (barre des tâches, Alt+Tab), les sortantes partent
        // derrière elle au lieu de repasser une dernière fois par-dessus.
        let front = t.focus.map(hwnd_of).filter(|h| !win::is_minimized(*h));
        self.show_overlay_below(s, front);
        self.hide(&t.minimize);
        self.screens[s].switch = Some(Switch {
            start: now,
            duration: self.anim_duration,
            flights,
            restore: t.restore.clone(),
            focus: t.focus,
            quiet,
            landed: None,
            settled: None,
        });
        true
    }

    fn show_overlay(&self, s: usize) {
        self.show_overlay_below(s, None);
    }

    /// Montre l'overlay des animations au premier plan, ou juste sous la fenêtre `front`.
    fn show_overlay_below(&self, s: usize, front: Option<HWND>) {
        let sc = &self.screens[s];
        let mon = sc.rect;
        unsafe {
            // Un pixel de moins que l'écran : sinon Windows croit à une appli plein écran
            // et demande à la barre de se cacher.
            let _ = SetWindowPos(
                sc.overlay,
                front.unwrap_or(HWND_TOPMOST),
                mon.left,
                mon.top,
                mon.right - mon.left,
                mon.bottom - mon.top - 1,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
            let _ = DwmFlush();
        }
    }

    fn hide_overlay(&self, s: usize) {
        unsafe {
            let _ = ShowWindow(self.screens[s].overlay, SW_HIDE);
        }
    }

    fn animating(&self) -> bool {
        self.screens
            .iter()
            .any(|sc| sc.switch.is_some() || sc.slide_start.is_some() || sc.hover_start.is_some() || sc.veil_start.is_some())
            || self.dock_animating()
            || self.notch_animating()
    }

    /// Une image d'animation, appelée à chaque rafraîchissement de l'écran.
    fn tick(&mut self) {
        let now = Instant::now();
        for s in 0..self.screens.len() {
            self.tick_screen(s, now);
        }
        self.dock_tick(now);
        self.notch_tick(now);
    }

    fn tick_screen(&mut self, s: usize, now: Instant) {
        let duration = self.anim_duration;
        let sc = &mut self.screens[s];
        // La barre se redessine tant que ses cartes bougent ou s'enchaînent en fondu.
        let mut redraw = sc.slide_start.is_some() || sc.hover_start.is_some() || sc.veil_start.is_some();
        if sc.slide_start.is_some_and(|start| anim::progress(start, duration, now) >= 1.0) {
            sc.slide_start = None;
        }
        // Le grossissement de l'icône survolée, et la pastille qui rentre ou sort du bord.
        if sc.hover_start.is_some_and(|start| anim::progress(start, HOVER_ANIM, now) >= 1.0) {
            sc.hover_start = None;
        }
        if sc.veil_start.is_some_and(|start| anim::progress(start, VEIL_ANIM, now) >= 1.0) {
            sc.veil_start = None;
        }
        let (mut land, mut finish) = (false, false);
        if let Some(switch) = &mut sc.switch {
            match switch.landed {
                None => {
                    let p = anim::progress(switch.start, switch.duration, now);
                    let e = anim::spring(p);
                    for f in &switch.flights {
                        // Les entrantes apparaissent en fondu ; les sortantes laissent la place
                        // à leur carte inclinée en fin de trajet.
                        let opacity = if f.fade_in {
                            anim::fade_in(p)
                        } else {
                            ((1.0 - anim::cross_fade(p)) * 255.0) as u8
                        };
                        anim::place_thumb(f.thumb, anim::lerp(f.from, f.to, e), true, opacity);
                    }
                    redraw |= sc.arriving.is_some() || sc.departing.is_some();
                    land = p >= 1.0;
                }
                Some(landed) => {
                    // Windows restaure les fenêtres en différé : on attend qu'elles soient
                    // vraiment revenues sous leurs miniatures.
                    if switch.settled.is_none() {
                        let back = switch.restore.iter().chain(&switch.focus).all(|&w| !win::is_minimized(hwnd_of(w)));
                        if back || now.duration_since(landed) >= SETTLE_MAX {
                            switch.settled = Some(now);
                        }
                    }
                    // Puis les miniatures s'effacent en fondu sur les vraies fenêtres.
                    if let Some(fade) = switch.settled.map(|t| t + SETTLE).filter(|t| now >= *t) {
                        let p = anim::progress(fade, SETTLE_FADE, now);
                        let opacity = ((1.0 - anim::ease_out(p)) * 255.0) as u8;
                        for f in switch.flights.iter().filter(|f| f.fade_in) {
                            anim::place_thumb(f.thumb, f.to, true, opacity);
                        }
                        finish = p >= 1.0;
                    }
                }
            }
        }
        if land {
            self.land(s, now);
        }
        if finish {
            self.finish_switch(s);
        }
        if redraw {
            self.render(s);
        }
    }

    /// Fin du trajet : les vraies fenêtres reprennent la place de leurs miniatures.
    fn land(&mut self, s: usize, now: Instant) {
        let Some(switch) = self.screens[s].switch.as_mut().filter(|sw| sw.landed.is_none()) else { return };
        switch.landed = Some(now);
        // Gardées : on surveille leur retour avant d'effacer les miniatures.
        let restore = switch.restore.clone();
        let focus = switch.focus;
        self.reveal(&restore, focus);
        self.screens[s].departing = None;
        self.reveal_arriving(s);
    }

    /// Termine tout de suite l'animation en cours sur cet écran, s'il y en a une.
    fn finish_switch(&mut self, s: usize) {
        self.land(s, Instant::now());
        let Some(switch) = self.screens[s].switch.take() else { return };
        unsafe {
            for f in switch.flights {
                let _ = DwmUnregisterThumbnail(f.thumb);
            }
        }
        // L'overlay porte aussi la carte qu'on est en train de déplacer.
        if !self.dragging(s) {
            self.hide_overlay(s);
        }
        self.apply_placements(s);
        for w in switch.quiet {
            anim::set_transitions(hwnd_of(w), true);
        }
    }

    fn reveal_arriving(&mut self, s: usize) {
        if self.screens[s].arriving.take().is_some() {
            self.render(s);
        }
    }

    fn reveal(&mut self, restore: &[Wid], focus: Option<Wid>) {
        for &w in restore {
            self.pending_minimize.remove(&w);
            let h = hwnd_of(w);
            if win::is_minimized(h) {
                win::show(h, SW_SHOWNOACTIVATE);
            }
        }
        if let Some(w) = focus {
            self.pending_minimize.remove(&w);
            let h = hwnd_of(w);
            if win::is_minimized(h) {
                win::show(h, SW_RESTORE);
            }
            win::force_foreground(h);
        }
    }

    fn hide(&mut self, minimize: &[Wid]) {
        for &w in minimize {
            let h = hwnd_of(w);
            if !win::is_minimized(h) {
                self.pending_minimize.insert(w);
                self.last_rects.insert(w, anim::window_rect(h));
                win::show(h, SW_SHOWMINNOACTIVE);
            }
        }
    }

    // --- Barre des scènes -----------------------------------------------------------

    /// Inscrit la barre d'un écran auprès de Windows, sans réserver de place (sauf la bande de
    /// l'encoche, voir `reserve_notch_band`) : elle apprend ainsi qu'une appli passe en plein
    /// écran, ce qui sert aussi aux encoches et aux coins.
    fn register_appbar(&mut self, s: usize) {
        let mut abd = APPBARDATA {
            cbSize: size_of::<APPBARDATA>() as u32,
            hWnd: self.screens[s].hwnd,
            uCallbackMessage: WM_APPBAR,
            ..Default::default()
        };
        unsafe { SHAppBarMessage(ABM_NEW, &mut abd) };
    }

    fn unregister_appbar(&mut self, s: usize) {
        self.screens[s].appbar = None;
        self.screens[s].notch_band = None;
        let mut abd = APPBARDATA { cbSize: size_of::<APPBARDATA>() as u32, hWnd: self.screens[s].hwnd, ..Default::default() };
        unsafe { SHAppBarMessage(ABM_REMOVE, &mut abd) };
    }

    /// Met en place la barre des scènes d'un écran.
    fn reserve_bar(&mut self, s: usize) {
        if self.screens[s].appbar.is_none() {
            self.screens[s].appbar = Some(RECT::default());
            self.place_appbar(s);
        }
    }

    /// Retire la barre des scènes, en restant inscrit pour le plein écran.
    fn release_bar(&mut self, s: usize) {
        self.screens[s].appbar = None;
    }

    /// Place la barre en haut de son écran : collée à gauche de l'encoche, ou centrée quand il
    /// n'y en a pas. Elle ne réserve aucune place : les fenêtres passent dessous.
    fn place_appbar(&mut self, s: usize) {
        let Some(current) = self.screens[s].appbar else { return };
        let edge = self.notch_edge(s);
        let sc = &mut self.screens[s];
        let dpi = sc.dpi;
        let px = move |v: i32| v * dpi / 96;
        let (w, h) = (px(BAR_WIN_W), px(BAR_WIN_H));
        let cx = (sc.rect.left + sc.rect.right) / 2;
        // Voir `pill_rect` : le bord droit de la pastille est à `NOTCH_GAP` de l'encoche.
        let left = match edge {
            Some(half) => cx - half - px(NOTCH_GAP) - w + px(PILL_SHOULDER) + px(2),
            None => cx - w / 2,
        };
        let r = RECT { left, top: sc.rect.top, right: left + w, bottom: sc.rect.top + h };
        if sc.bar_anchored != edge.is_some() {
            sc.bar_anchored = edge.is_some();
            sc.layout_key.clear();
        }
        if r != current {
            sc.appbar = Some(r);
            unsafe {
                let _ = SetWindowPos(sc.hwnd, HWND_TOPMOST, r.left, r.top, w, h, SWP_NOACTIVATE);
                if size(r) != size(current) {
                    // L'échelle a changé. Le Dock se sert aussi de cette police pour ses libellés.
                    if !sc.font.is_invalid() {
                        let _ = DeleteObject(sc.font);
                    }
                    // Inter, la police libre la plus proche de celle d'Apple (voir `load_fonts`).
                    sc.font = make_font("Inter", 12, 400, dpi);
                }
            }
        }
        self.refresh(s);
        self.render(s);
    }

    /// L'encoche a changé (largeur, ouverte ou fermée, activée ou non) : la barre la suit, et
    /// rentre dans le bord de l'écran tant que l'encoche est ouverte par-dessus.
    pub(super) fn follow_notch(&mut self, s: usize) {
        let open = self.notch_open(s);
        let now = Instant::now();
        let shown = self.bar_shown(s, now);
        let sc = &mut self.screens[s];
        if sc.veiled != open {
            sc.veiled = open;
            sc.veil_from = shown;
            sc.veil_start = Some(now);
        }
        self.place_appbar(s);
    }

    /// Part visible de la pastille : 0 rentrée dans le bord de l'écran, 1 sortie.
    fn bar_shown(&self, s: usize, now: Instant) -> f64 {
        let sc = &self.screens[s];
        let target = if sc.veiled { 0.0 } else { 1.0 };
        match sc.veil_start {
            Some(start) => sc.veil_from + (target - sc.veil_from) * anim::ease_out(anim::progress(start, VEIL_ANIM, now)),
            None => target,
        }
    }

    fn px(&self, s: usize, v: i32) -> i32 {
        v * self.screens[s].dpi / 96
    }

    /// Largeur de la pastille pour `n` icônes.
    fn pill_width(&self, s: usize, n: usize) -> i32 {
        if n == 0 {
            return 0;
        }
        let n = n as i32;
        n * self.px(s, ICON) + (n - 1) * self.px(s, ICON_GAP) + 2 * self.px(s, PILL_PAD)
    }

    /// Largeur de la pastille en ce moment : elle s'allonge et se resserre avec les icônes.
    fn pill_shown_width(&self, s: usize, now: Instant) -> i32 {
        let sc = &self.screens[s];
        let e = anim::spring(self.slide_progress(s, now));
        sc.pill_from + ((sc.pill_to - sc.pill_from) as f64 * e).round() as i32
    }

    /// La pastille, large de `width`, dans la fenêtre de la barre : calée à droite contre
    /// l'encoche, ou centrée.
    fn pill_rect(&self, s: usize, width: i32) -> RECT {
        let sc = &self.screens[s];
        let w = sc.appbar.map_or(0, |b| b.right - b.left);
        let right = if sc.bar_anchored { w - self.px(s, PILL_SHOULDER) - self.px(s, 2) } else { (w + width) / 2 };
        RECT { left: right - width, top: 0, right, bottom: self.px(s, BAR_H) }
    }

    /// Emplacement de la `i`-ième icône sur `n` : la scène la plus récente près de l'encoche.
    fn icon_slot(&self, s: usize, i: usize, n: usize) -> RECT {
        let pill = self.pill_rect(s, self.pill_width(s, n.max(1)));
        let (icon, gap) = (self.px(s, ICON), self.px(s, ICON_GAP));
        let right = pill.right - self.px(s, PILL_PAD) - i as i32 * (icon + gap);
        let top = (pill.bottom - icon) / 2;
        RECT { left: right - icon, top, right, bottom: top + icon }
    }

    fn add_tray(&self) {
        self.tray(NIM_ADD);
    }

    fn remove_tray(&self) {
        self.tray(NIM_DELETE);
    }

    fn tray(&self, message: NOTIFY_ICON_MESSAGE) {
        let mut nid = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: 1,
            uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
            uCallbackMessage: WM_TRAY,
            hIcon: win::app_icon(),
            ..Default::default()
        };
        for (i, c) in "Stage Manager".encode_utf16().enumerate() {
            nid.szTip[i] = c;
        }
        unsafe {
            let _ = Shell_NotifyIconW(message, &nid);
        }
    }

    /// Icône d'une fenêtre : la grande icône de son appli, sinon celle de la fenêtre.
    fn icon(&mut self, w: Wid) -> Rc<Image> {
        if let Some(icon) = self.icons.get(&w) {
            return Rc::clone(icon);
        }
        let h = hwnd_of(w);
        let path = win::process_path(win::app_pid(h));
        let icon = match path.as_ref().and_then(|p| self.app_icons.get(p)) {
            Some(icon) => Rc::clone(icon),
            None => {
                let mut image = path.as_deref().and_then(shell::big_icon).unwrap_or_else(|| hicon_image(win::icon(h), 64));
                // Réduite d'avance : dessinée en tout petit, une image de 128 px scintillerait.
                while image.w > 64 {
                    image = image.half();
                }
                let icon = Rc::new(image);
                if let Some(p) = path {
                    self.app_icons.insert(p, Rc::clone(&icon));
                }
                icon
            }
        };
        self.icons.insert(w, Rc::clone(&icon));
        icon
    }

    fn refresh_all(&mut self) {
        for s in 0..self.screens.len() {
            self.refresh(s);
        }
    }

    fn render_all(&self) {
        for s in 0..self.screens.len() {
            self.render(s);
        }
    }

    /// Recalcule la place des icônes de la barre d'un écran.
    fn refresh(&mut self, s: usize) {
        if self.screens[s].appbar.is_none() {
            return;
        }
        let screen = self.screens[s].id;
        let stages: Vec<(u64, Wid)> =
            self.model.sidebar(screen).iter().take(MAX_ICONS as usize).map(|st| (st.id, st.windows[0])).collect();
        let n = stages.len();
        let layout: LayoutKey =
            stages.iter().enumerate().map(|(i, &(stage, window))| (stage, window, n, self.icon_slot(s, i, n))).collect();
        if layout == self.screens[s].layout_key {
            return;
        }

        // Les icônes qui restent glissent depuis l'endroit où elles sont affichées en ce moment.
        let now = Instant::now();
        let previous: HashMap<u64, RECT> = self.screens[s]
            .cards
            .iter()
            .map(|c| {
                let (dx, dy) = self.card_offset(s, c, now);
                (c.stage, offset(c.dest, dx, dy))
            })
            .collect();
        let arriving = self.screens[s].arriving;
        let (half_gap, bar_h) = (self.px(s, ICON_GAP) / 2, self.px(s, BAR_H));
        let mut cards = Vec::new();
        let mut sliding = false;
        for &(stage, window, _, slot) in &layout {
            let slide = previous.get(&stage).map_or((0, 0), |r| (r.left - slot.left, r.top - slot.top));
            // Une icône qui n'était pas affichée (et ne vient pas d'un trajet) surgit.
            let fade_in = !previous.contains_key(&stage) && arriving != Some(stage);
            sliding |= slide != (0, 0) || fade_in;
            let icon = self.icon(window);
            let rect = RECT { left: slot.left - half_gap, top: 0, right: slot.right + half_gap, bottom: bar_h };
            cards.push(Card { stage, window, rect, thumb_box: slot, dest: slot, icon, slide, hidden: false, fade_in });
        }
        let pill_from = self.pill_shown_width(s, now);
        let pill_to = self.pill_width(s, n);
        sliding |= pill_from != pill_to;
        let sc = &mut self.screens[s];
        sc.cards = cards;
        sc.pill_from = pill_from;
        sc.pill_to = pill_to;
        sc.slide_start = sliding.then_some(now);
        sc.layout_key = layout;
        if sc.hover.is_some_and(|h| !sc.cards.iter().any(|c| c.stage == h)) {
            sc.hover = None;
        }
        self.render(s);
    }

    /// Décalage d'affichage d'une icône pendant l'animation de glissement.
    fn card_offset(&self, s: usize, card: &Card, now: Instant) -> (i32, i32) {
        let remaining = 1.0 - anim::spring(self.slide_progress(s, now));
        ((card.slide.0 as f64 * remaining) as i32, (card.slide.1 as f64 * remaining) as i32)
    }

    fn slide_progress(&self, s: usize, now: Instant) -> f64 {
        self.screens[s].slide_start.map_or(1.0, |start| anim::progress(start, self.anim_duration, now))
    }

    /// Taille de la fenêtre là où elle reviendra.
    fn window_size(&self, w: Wid) -> SIZE {
        let rect = self.last_rects.get(&w).copied().unwrap_or_else(|| anim::restored_rect(hwnd_of(w)));
        size(rect)
    }

    /// Opacité d'une icône : fondu d'apparition, fondu enchaîné avec les fenêtres qui la
    /// rejoignent, ou invisible pendant qu'on la déplace.
    fn card_alpha(&self, s: usize, card: &Card, now: Instant) -> u8 {
        let sc = &self.screens[s];
        if sc.arriving == Some(card.stage) {
            let p = sc.switch.as_ref().map_or(1.0, |sw| anim::progress(sw.start, sw.duration, now));
            return (anim::cross_fade(p) * 255.0) as u8;
        }
        if card.hidden {
            return 0;
        }
        if card.fade_in {
            return anim::fade_in(self.slide_progress(s, now));
        }
        255
    }

    /// Taille d'une icône : elle grossit au survol (avec un petit rebond), surgit en arrivant
    /// et s'enfonce sous le clic.
    fn icon_scale(&self, s: usize, card: &Card, now: Instant) -> f64 {
        let sc = &self.screens[s];
        let e = sc.hover_start.map_or(1.0, |start| anim::spring(anim::progress(start, HOVER_ANIM, now)));
        let grown = if sc.hover == Some(card.stage) {
            e
        } else if sc.hover_prev == Some(card.stage) {
            1.0 - e
        } else {
            0.0
        };
        let mut scale = 1.0 + HOVER_SCALE * grown;
        if card.fade_in {
            scale *= 0.3 + 0.7 * anim::spring(self.slide_progress(s, now));
        }
        if sc.drag.as_ref().is_some_and(|d| d.stage == card.stage && d.ghost.is_none()) {
            scale *= 0.85;
        }
        scale
    }

    fn hit(&self, s: usize, pt: POINT) -> Option<u64> {
        self.screens[s].cards.iter().find(|c| contains(c.rect, pt)).map(|c| c.stage)
    }

    /// Scène sous la souris dans la barre `hwnd` (clic droit).
    fn bar_hit(&self, hwnd: HWND, pt: POINT) -> Option<u64> {
        self.hit(self.bar_index(hwnd)?, pt)
    }

    fn on_mouse_move(&mut self, s: usize, pt: POINT) {
        let sc = &mut self.screens[s];
        if !sc.tracking_mouse {
            let mut tme = TRACKMOUSEEVENT {
                cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: sc.hwnd,
                dwHoverTime: 0,
            };
            sc.tracking_mouse = unsafe { TrackMouseEvent(&mut tme) }.is_ok();
        }
        self.set_hover(s, self.hit(s, pt));
    }

    fn set_hover(&mut self, s: usize, hover: Option<u64>) {
        let sc = &mut self.screens[s];
        if sc.hover != hover {
            sc.hover_prev = sc.hover;
            sc.hover = hover;
            sc.hover_start = Some(Instant::now());
            self.render(s);
        }
    }

    // --- Glisser-déposer ------------------------------------------------------------

    fn dragging(&self, s: usize) -> bool {
        self.screens[s].drag.as_ref().is_some_and(|d| d.ghost.is_some())
    }

    fn press(&mut self, s: usize, pt: POINT) {
        let sc = &self.screens[s];
        let Some(bar) = sc.appbar else { return };
        let Some(card) = sc.cards.iter().find(|c| !c.hidden && contains(c.rect, pt)) else { return };
        let mon = sc.rect;
        let (dx, dy) = self.card_offset(s, card, Instant::now());
        let icon = offset(card.dest, dx + bar.left - mon.left, dy + bar.top - mon.top);
        let origin = fit(icon, self.window_size(card.window));
        let drag = Drag { stage: card.stage, window: card.window, start: pt, origin, ghost: None, rect: origin };
        self.screens[s].drag = Some(drag);
        unsafe {
            // Garde la souris même quand elle sort de la barre.
            SetCapture(self.screens[s].hwnd);
        }
        // L'icône s'enfonce sous le clic.
        self.render(s);
    }

    fn drag_to(&mut self, s: usize, pt: POINT) {
        let Some(bar) = self.screens[s].appbar else { return };
        let Some(start) = self.screens[s].drag.as_ref().map(|d| d.start) else { return };
        if !self.dragging(s) {
            // Un petit tremblement pendant un clic ne doit pas lancer un glisser.
            let threshold = unsafe { GetSystemMetrics(SM_CXDRAG) }.max(4);
            if (pt.x - start.x).abs() < threshold && (pt.y - start.y).abs() < threshold {
                return;
            }
            self.begin_drag(s);
        }
        let (mon, bar_h) = (self.screens[s].rect, self.px(s, BAR_H));
        let Some(drag) = self.screens[s].drag.as_mut() else { return };
        let Some(ghost) = drag.ghost else { return };
        let to_overlay = |p: POINT| POINT { x: p.x + bar.left - mon.left, y: p.y + bar.top - mon.top };
        let (cursor, grab) = (to_overlay(pt), to_overlay(drag.start));
        // La fenêtre grandit à mesure qu'on l'éloigne de la barre.
        let away = (cursor.y - (bar.top - mon.top + bar_h)).max(0) as f64;
        let scale = 1.0 + (away / 300.0).min(1.0) * 9.0;
        let scaled = |v: i32| (v as f64 * scale) as i32;
        let o = drag.origin;
        let left = cursor.x - scaled(grab.x - o.left);
        let top = cursor.y - scaled(grab.y - o.top);
        drag.rect = RECT { left, top, right: left + scaled(o.right - o.left), bottom: top + scaled(o.bottom - o.top) };
        anim::place_thumb(ghost, drag.rect, true, 235);
    }

    fn begin_drag(&mut self, s: usize) {
        self.finish_switch(s);
        let overlay = self.screens[s].overlay;
        let Some(drag) = self.screens[s].drag.as_mut() else { return };
        let Ok(ghost) = (unsafe { DwmRegisterThumbnail(overlay, hwnd_of(drag.window)) }) else { return };
        anim::place_thumb(ghost, drag.origin, true, 235);
        drag.ghost = Some(ghost);
        let stage = drag.stage;
        // L'icône quitte la barre pendant qu'on la déplace.
        self.set_card_hidden(s, stage, true);
        self.screens[s].hover = None;
        self.show_overlay(s);
        self.render(s);
    }

    fn release(&mut self, s: usize, pt: POINT) {
        let Some(drag) = self.screens[s].drag.take() else { return };
        unsafe {
            let _ = ReleaseCapture();
        }
        let Some(ghost) = drag.ghost else {
            self.render(s);
            // Simple clic : Maj ajoute la scène à la scène active.
            if self.hit(s, pt) == Some(drag.stage) {
                let merge = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
                self.show_stage(drag.stage, merge);
            }
            return;
        };
        if pt.y >= self.px(s, BAR_H) {
            // Lâchée sous la barre : la scène rejoint la scène active, en partant d'ici.
            self.screens[s].drag_source = Some(drag.rect);
            self.show_stage(drag.stage, true);
            self.screens[s].drag_source = None;
        }
        self.drop_ghost(s, ghost, drag.stage);
    }

    fn cancel_drag(&mut self, s: usize) {
        let Some(drag) = self.screens[s].drag.take() else { return };
        if let Some(ghost) = drag.ghost {
            self.drop_ghost(s, ghost, drag.stage);
        }
    }

    fn drop_ghost(&mut self, s: usize, ghost: isize, stage: u64) {
        unsafe {
            let _ = DwmUnregisterThumbnail(ghost);
        }
        if self.screens[s].switch.is_none() {
            self.hide_overlay(s);
        }
        // Lâchée sur la barre : l'icône reprend sa place.
        self.set_card_hidden(s, stage, false);
    }

    fn set_card_hidden(&mut self, s: usize, stage: u64, hidden: bool) {
        for card in self.screens[s].cards.iter_mut().filter(|c| c.stage == stage) {
            card.hidden = hidden;
        }
        self.render(s);
    }

    /// Redessine la barre d'un écran : une pastille noire et ses icônes, sans nom ni aperçu.
    /// Autour, tout est transparent et laisse passer les clics.
    fn render(&self, s: usize) {
        let sc = &self.screens[s];
        let Some(bar) = sc.appbar else { return };
        let (w, h) = (bar.right - bar.left, bar.bottom - bar.top);
        if w <= 0 || h <= 0 {
            return;
        }
        let px = |v: i32| self.px(s, v);
        let now = Instant::now();
        let shown = self.bar_shown(s, now);
        let drop_target = self.drop_target == Some(sc.id);
        let mut width = self.pill_shown_width(s, now);
        if drop_target {
            // Une fenêtre est traînée au-dessus : la pastille s'éclaire, même vide.
            width = width.max(self.pill_width(s, 1));
        }

        let mut canvas = Canvas::new(w, h);
        if shown > 0.0 && width > 0 {
            // Rentrée, la pastille remonte dans le bord de l'écran.
            let lift = ((1.0 - shown) * px(BAR_H + 4) as f64).round() as i32;
            let pill = offset(self.pill_rect(s, width), 0, -lift);
            let fill = if drop_target { (78, 78, 86) } else { (0, 0, 0) };
            canvas.notch(pill, px(PILL_RADIUS) as f32, px(PILL_SHOULDER) as f32, fill, 255);

            // (icône, décalage, opacité)
            let mut icons: Vec<(&Card, (i32, i32), u8)> =
                sc.cards.iter().map(|c| (c, self.card_offset(s, c, now), self.card_alpha(s, c, now))).collect();
            // L'icône de la scène qui revient à l'écran s'efface pendant que ses fenêtres partent.
            if let (Some(card), Some(switch)) = (&sc.departing, &sc.switch) {
                icons.push((card, (0, 0), 255 - anim::fade_in(anim::progress(switch.start, switch.duration, now))));
            }
            for (card, (dx, dy), alpha) in icons {
                let alpha = (alpha as f64 * shown) as u8;
                if alpha == 0 {
                    continue;
                }
                let r = offset(card.dest, dx, dy - lift);
                let half = (r.right - r.left) as f64 * self.icon_scale(s, card, now) / 2.0;
                let (cx, cy) = ((r.left + r.right) as f64 / 2.0, (r.top + r.bottom) as f64 / 2.0);
                let (l, t, rr, b) = ((cx - half) as f32, (cy - half) as f32, (cx + half) as f32, (cy + half) as f32);
                canvas.draw_quad(&card.icon, [(l, t), (rr, t), (rr, b), (l, b)], alpha, 0);
            }
        }
        canvas.present(sc.hwnd, POINT { x: bar.left, y: bar.top });
    }
}

// --- Géométrie et dessin -------------------------------------------------------------

/// Dessine une icône Windows dans une image, pour pouvoir l'agrandir et la fondre.
fn hicon_image(icon: HICON, size: i32) -> Image {
    let mut canvas = Canvas::new(size, size);
    canvas.gdi(|dc| unsafe {
        let _ = DrawIconEx(dc, 0, 0, icon, size, size, 0, None, DI_NORMAL);
    });
    canvas.into_image()
}

fn contains(r: RECT, pt: POINT) -> bool {
    pt.x >= r.left && pt.x < r.right && pt.y >= r.top && pt.y < r.bottom
}

fn point(lp: LPARAM) -> POINT {
    POINT { x: (lp.0 & 0xFFFF) as i16 as i32, y: ((lp.0 >> 16) & 0xFFFF) as i16 as i32 }
}

/// Charge Inter (intégrée à l'exécutable) pour ce seul processus : rien n'est installé dans
/// Windows. Elle apparaît sous deux noms : « Inter » et « Inter SemiBold ».
fn load_fonts() {
    const FONTS: [&[u8]; 2] = [
        include_bytes!("../assets/fonts/Inter-Regular.ttf"),
        include_bytes!("../assets/fonts/Inter-SemiBold.ttf"),
    ];
    for font in FONTS {
        let mut count = 0u32;
        unsafe {
            AddFontMemResourceEx(font.as_ptr().cast(), font.len() as u32, None, &mut count);
        }
    }
}

unsafe fn make_font(face: &str, points: i32, weight: i32, dpi: i32) -> HFONT {
    let mut lf = LOGFONTW {
        lfHeight: -points * dpi / 96,
        lfWeight: weight,
        // Lissage en niveaux de gris : ClearType donne des franges colorées sur fond transparent.
        lfQuality: ANTIALIASED_QUALITY,
        ..Default::default()
    };
    for (i, c) in face.encode_utf16().take(31).enumerate() {
        lf.lfFaceName[i] = c;
    }
    CreateFontIndirectW(&lf)
}

/// Texte avec une ombre portée, pour rester lisible sur n'importe quel fond d'écran.
unsafe fn draw_text(dc: HDC, text: &str, rect: RECT, flags: DRAW_TEXT_FORMAT, color: COLORREF, shadow: i32) {
    let mut text: Vec<u16> = text.encode_utf16().collect();
    if shadow > 0 {
        let mut shadow_rect = offset(rect, shadow, shadow);
        SetTextColor(dc, rgb(0, 0, 0));
        DrawTextW(dc, &mut text, &mut shadow_rect, flags);
    }
    let mut rect = rect;
    SetTextColor(dc, color);
    DrawTextW(dc, &mut text, &mut rect, flags);
}

// --- Boucle Win32 --------------------------------------------------------------------

/// Accès à l'état, sauf si on y est déjà (appel réentrant pendant un message envoyé).
fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|cell| cell.try_borrow_mut().ok().and_then(|mut app| app.as_mut().map(f)))
}

unsafe extern "system" fn win_event(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    id_object: i32,
    id_child: i32,
    _thread: u32,
    _time: u32,
) {
    // On ne garde que les fenêtres elles-mêmes (OBJID_WINDOW, CHILDID_SELF).
    let wanted = matches!(
        event,
        EVENT_SYSTEM_FOREGROUND
            | EVENT_SYSTEM_MINIMIZESTART
            | EVENT_SYSTEM_MINIMIZEEND
            | EVENT_SYSTEM_MOVESIZESTART
            | EVENT_SYSTEM_MOVESIZEEND
            | EVENT_OBJECT_HIDE
            | EVENT_OBJECT_DESTROY
    );
    if !wanted || hwnd.is_invalid() || id_object != 0 || id_child != 0 {
        return;
    }
    // Traité plus tard dans la boucle de messages, jamais en plein milieu d'un autre traitement.
    let main = HWND(MAIN.load(Ordering::Relaxed) as _);
    let _ = PostMessageW(main, WM_WINEVENT, WPARAM(event as usize), LPARAM(hwnd.0 as isize));
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let is_dock = hwnd.0 as isize == DOCK.load(Ordering::Relaxed);
    let is_launcher = hwnd.0 as isize == LAUNCHER.load(Ordering::Relaxed);
    if hwnd.0 as isize == SETTINGS.load(Ordering::Relaxed) {
        // Une vraie fenêtre, qui prend le premier plan comme les autres.
        if msg == WM_MOUSEACTIVATE {
            return LRESULT(MA_ACTIVATE as isize);
        }
        return match with_app(|app| app.settings_input(msg, wp, lp)) {
            Some(Some(result)) => result,
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        };
    }
    match msg {
        // Le lanceur, lui, prend le premier plan : il lui faut le clavier.
        WM_CHAR | WM_KEYDOWN | WM_ACTIVATE | WM_MOUSEMOVE | WM_LBUTTONUP if is_launcher => {
            with_app(|app| app.launcher_input(msg, wp, lp));
            return DefWindowProcW(hwnd, msg, wp, lp);
        }
        WM_MOUSEACTIVATE if is_launcher => return LRESULT(MA_ACTIVATE as isize),
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        WM_ERASEBKGND => return LRESULT(1),
        WM_RBUTTONUP if is_dock => {
            dockbar::dock_menu(hwnd, point(lp));
            return LRESULT(0);
        }
        WM_MOUSEMOVE | WM_MOUSELEAVE | WM_LBUTTONDOWN | WM_LBUTTONUP | WM_MOUSEWHEEL if is_dock => {
            with_app(|app| app.dock_mouse(msg, wp, lp));
            return LRESULT(0);
        }
        // Les menus ont leur propre boucle de messages : on les ouvre hors de `with_app`.
        WM_TRAY => {
            let mouse = (lp.0 & 0xFFFF) as u32;
            if mouse == WM_RBUTTONUP || mouse == WM_CONTEXTMENU {
                tray_menu(hwnd);
            } else if mouse == WM_LBUTTONUP {
                with_app(App::open_settings);
            }
            return LRESULT(0);
        }
        _ => {}
    }
    match with_app(|app| app.handle(msg, wp, lp)) {
        Some(Some(result)) => result,
        Some(None) => DefWindowProcW(hwnd, msg, wp, lp),
        None => {
            // Occupé : on remet l'événement à plus tard plutôt que de le perdre.
            if [WM_WINEVENT, WM_MEDIA, WM_DOCKBAR, WM_DOCK_LOADED, WM_DOCK_WALLPAPER, WM_LAUNCHER_APPS, WM_LAUNCHER_ICON]
                .contains(&msg)
            {
                let _ = PostMessageW(hwnd, msg, wp, lp);
                return LRESULT(0);
            }
            DefWindowProcW(hwnd, msg, wp, lp)
        }
    }
}

/// Barre des scènes d'un écran.
unsafe extern "system" fn bar_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_ERASEBKGND => LRESULT(1),
        // Le menu a sa propre boucle de messages : on l'ouvre hors de `with_app`.
        WM_RBUTTONUP => {
            if let Some(Some(stage)) = with_app(|app| app.bar_hit(hwnd, point(lp))) {
                card_menu(hwnd, stage);
            }
            LRESULT(0)
        }
        WM_APPBAR | WM_MOUSEMOVE | WM_MOUSELEAVE | WM_LBUTTONDOWN | WM_LBUTTONUP | WM_CAPTURECHANGED => {
            // Occupé : on remet l'avis de Windows à plus tard plutôt que de le perdre.
            if with_app(|app| app.bar_message(hwnd, msg, wp, lp)).is_none() && msg == WM_APPBAR {
                let _ = PostMessageW(hwnd, msg, wp, lp);
            }
            LRESULT(0)
        }
        WM_DISPLAYCHANGE | WM_DPICHANGED => {
            screens_changed();
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// Encoche d'un écran.
unsafe extern "system" fn notch_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_ERASEBKGND => LRESULT(1),
        WM_RBUTTONUP => {
            notch::notch_menu(hwnd);
            LRESULT(0)
        }
        WM_MOUSEMOVE | WM_MOUSELEAVE | WM_LBUTTONDOWN | WM_LBUTTONUP => {
            with_app(|app| app.notch_mouse(hwnd, msg, lp));
            LRESULT(0)
        }
        WM_TIMER => {
            with_app(|app| app.notch_timer(hwnd, wp.0));
            LRESULT(0)
        }
        WM_DISPLAYCHANGE | WM_DPICHANGED => {
            screens_changed();
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// Menu contextuel ; `checked` liste les entrées à cocher. Renvoie l'entrée choisie (0 si aucune).
unsafe fn popup(hwnd: HWND, items: &[(usize, PCWSTR)], checked: &[usize]) -> usize {
    let Ok(menu) = CreatePopupMenu() else { return 0 };
    for &(id, label) in items {
        let mut flags = if id == 0 { MF_SEPARATOR } else { MF_STRING };
        if checked.contains(&id) {
            flags |= MF_CHECKED;
        }
        let _ = AppendMenuW(menu, flags, id, label);
    }
    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);
    let previous = GetForegroundWindow();
    // Sans ça, le menu ne se ferme pas quand on clique ailleurs.
    let _ = SetForegroundWindow(hwnd);
    let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY, pt.x, pt.y, 0, hwnd, None).0 as usize;
    let _ = DestroyMenu(menu);
    if cmd == 0 && !previous.is_invalid() {
        win::force_foreground(previous);
    }
    cmd
}

unsafe fn tray_menu(hwnd: HWND) {
    let autostart = autostart::is_enabled();
    let items = [
        (MENU_SETTINGS, w!("Réglages…")),
        (MENU_AUTOSTART, w!("Lancer au démarrage de Windows")),
        (0, PCWSTR::null()),
        (MENU_QUIT, w!("Quitter")),
    ];
    let checked = if autostart { vec![MENU_AUTOSTART] } else { vec![] };
    match popup(hwnd, &items, &checked) {
        MENU_SETTINGS => {
            with_app(App::open_settings);
        }
        MENU_AUTOSTART => {
            autostart::set(!autostart);
            with_app(App::refresh_settings);
        }
        MENU_QUIT => PostQuitMessage(0),
        _ => {}
    }
}

unsafe fn card_menu(hwnd: HWND, stage: u64) {
    let mut items = vec![(MENU_SHOW, w!("Afficher cette scène")), (MENU_MERGE, w!("Ajouter à la scène active\tMaj+clic"))];
    let windows = with_app(|app| app.model.stage(stage).map_or(0, |s| s.windows.len())).unwrap_or(0);
    if windows > 1 {
        items.push((MENU_SPLIT, w!("Séparer les fenêtres")));
    }
    match popup(hwnd, &items, &[]) {
        MENU_SHOW => with_app(|app| app.show_stage(stage, false)),
        MENU_MERGE => with_app(|app| app.show_stage(stage, true)),
        MENU_SPLIT => with_app(|app| {
            if app.model.split(stage) {
                app.refresh_all();
            }
        }),
        _ => None,
    };
}

pub fn run() -> Result<()> {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        load_fonts();
        // COM : le Dock lance des applis et ouvre la corbeille via le shell.
        let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
        let instance: HINSTANCE = GetModuleHandleW(None)?.into();
        // La fenêtre principale garde l'ancien nom de classe : une deuxième instance la trouve
        // pour ouvrir les réglages (voir `show_running_settings`).
        let class = w!("StageManagerSidebar");
        let wc = WNDCLASSW {
            hInstance: instance,
            lpszClassName: class,
            lpfnWndProc: Some(wndproc),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            style: CS_HREDRAW | CS_VREDRAW,
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            return Err(Error::from_win32());
        }
        // Jamais affichée : elle reçoit les événements, les minuteurs et l'icône de notification.
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class,
            w!("Stage Manager"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            instance,
            None,
        )?;
        MAIN.store(hwnd.0 as isize, Ordering::Relaxed);
        // Les barres des scènes et les encoches : une de chaque par écran (voir `screens`).
        let procs: [(PCWSTR, WNDPROC); 2] = [(BAR_CLASS, Some(bar_proc)), (NOTCH_CLASS, Some(notch_proc))];
        for (name, proc) in procs {
            let screen_wc = WNDCLASSW { lpszClassName: name, lpfnWndProc: proc, ..wc };
            if RegisterClassW(&screen_wc) == 0 {
                return Err(Error::from_win32());
            }
        }
        let dock_class = w!("StageManagerDock");
        let dock_wc = WNDCLASSW { lpszClassName: dock_class, ..wc };
        if RegisterClassW(&dock_wc) == 0 {
            return Err(Error::from_win32());
        }
        let dock = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            dock_class,
            w!("Dock"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            instance,
            None,
        )?;
        DOCK.store(dock.0 as isize, Ordering::Relaxed);
        let launcher_class = w!("StageManagerLauncher");
        let launcher_wc = WNDCLASSW { lpszClassName: launcher_class, ..wc };
        if RegisterClassW(&launcher_wc) == 0 {
            return Err(Error::from_win32());
        }
        let launcher = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            launcher_class,
            w!("Rechercher"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            instance,
            None,
        )?;
        LAUNCHER.store(launcher.0 as isize, Ordering::Relaxed);
        // Les réglages : une fenêtre ordinaire (barre des tâches, Alt+Tab), avec l'icône de l'appli.
        let settings_class = w!("StageManagerSettings");
        let settings_wc = WNDCLASSW { lpszClassName: settings_class, hIcon: win::app_icon(), ..wc };
        if RegisterClassW(&settings_wc) == 0 {
            return Err(Error::from_win32());
        }
        let settings = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_APPWINDOW,
            settings_class,
            w!("Réglages du Stage Manager"),
            WS_POPUP | WS_SYSMENU | WS_MINIMIZEBOX,
            0,
            0,
            0,
            0,
            None,
            None,
            instance,
            None,
        )?;
        SETTINGS.store(settings.0 as isize, Ordering::Relaxed);
        let corners = Corners::create(instance)?;
        APP.with(|cell| *cell.borrow_mut() = Some(App::new(hwnd, dock, launcher, settings, corners)));

        let flags = WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS;
        let hooks = [
            SetWinEventHook(EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_MINIMIZEEND, None, Some(win_event), 0, 0, flags),
            SetWinEventHook(EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE, None, Some(win_event), 0, 0, flags),
        ];
        let _ = RegisterHotKey(hwnd, HOTKEY_TOGGLE, MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, 'S' as u32);
        let _ = RegisterHotKey(hwnd, HOTKEY_DETACH, MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, 'D' as u32);
        // Ceux du lanceur et de la page accrochée suivent leur réglage (voir `apply_features`).
        with_app(|app| {
            app.add_tray();
            app.start();
        });

        let mut msg = MSG::default();
        'main: loop {
            if with_app(|app| app.animating()).unwrap_or(false) {
                // Pendant une animation : on vide la file puis on dessine une image,
                // calée sur le rafraîchissement de l'écran par DwmFlush.
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    if msg.message == WM_QUIT {
                        break 'main;
                    }
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                with_app(App::tick);
                let _ = DwmFlush();
            } else {
                if !GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    break;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }

        for hook in hooks {
            let _ = UnhookWinEvent(hook);
        }
        with_app(|app| {
            app.stop();
            app.remove_tray();
        });
        let _ = DestroyWindow(hwnd);
    }
    Ok(())
}
