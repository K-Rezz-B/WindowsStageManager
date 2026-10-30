//! Barre latérale, icône de notification et branchement sur les événements Windows.

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
    canvas::{Canvas, Glass, Image},
    media::{Media, NowPlaying},
    snapshot, taskbar, wallpaper,
    stages::{Model, Transition, Wid},
    win::{self, hwnd_of, wid_of},
};

mod dockbar;

use dockbar::{DockUi, DOCK, WM_DOCKBAR, WM_DOCK_LOADED, WM_DOCK_WALLPAPER};

const WM_WINEVENT: u32 = WM_APP + 1;
const WM_TRAY: u32 = WM_APP + 2;
const WM_APPBAR: u32 = WM_APP + 3;
const WM_MEDIA: u32 = WM_APP + 4;
const WM_WALLPAPER: u32 = WM_APP + 5;
/// Envoyé avec WM_SETTINGCHANGE quand le fond d'écran change.
const SPI_SETDESKWALLPAPER: usize = 0x0014;
const WM_MOUSELEAVE: u32 = 0x02A3;
const TIMER_RESCAN: usize = 1;
const TIMER_WINDOW_DRAG: usize = 2;
const HOTKEY_TOGGLE: i32 = 1;
const HOTKEY_DETACH: i32 = 2;
const MENU_TOGGLE: usize = 1;
const MENU_QUIT: usize = 2;
const MENU_SHOW: usize = 3;
const MENU_MERGE: usize = 4;
const MENU_AUTOSTART: usize = 5;
const MENU_SPLIT: usize = 6;

/// Largeur de la barre, en pixels à 100 %.
const BAR_WIDTH: i32 = 200;
/// Après un changement de scène, on ignore les événements parasites qu'il provoque.
const SWITCH_GUARD: Duration = Duration::from_millis(500);
/// Largeur des instantanés de fenêtres, en pixels (environ deux fois une carte, pour la netteté).
const SNAPSHOT_WIDTH: i32 = 400;
/// Durée par défaut des animations (modifiable avec STAGE_MANAGER_ANIM_MS, 0 pour les couper).
const ANIM_MS: u64 = 650;
/// Délai entre la restauration des vraies fenêtres et le retrait des miniatures animées :
/// le temps qu'elles se redessinent et se remettent dans le bon ordre, cachées dessous.
const SETTLE: Duration = Duration::from_millis(150);

const TEXT: COLORREF = rgb(245, 245, 248);
const TEXT_DIM: COLORREF = rgb(200, 200, 208);

static SIDEBAR: AtomicIsize = AtomicIsize::new(0);
/// Fenêtres réduites par nous, à restaurer quoi qu'il arrive en quittant.
static HIDDEN: Mutex<Vec<Wid>> = Mutex::new(Vec::new());

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16)
}

/// Restaure toutes les fenêtres qu'on a réduites (sortie, désactivation, panique).
pub fn restore_hidden() {
    let windows = std::mem::take(&mut *HIDDEN.lock().unwrap_or_else(|e| e.into_inner()));
    for w in windows {
        let h = hwnd_of(w);
        if unsafe { IsWindow(h).as_bool() } && win::is_minimized(h) {
            win::show(h, SW_SHOWNOACTIVATE);
        }
    }
    // Et la barre des tâches, si on l'avait passée en masquage automatique pour le Dock.
    taskbar::restore();
}

#[derive(Clone)]
struct Card {
    stage: u64,
    window: Wid,
    /// Fenêtres dessinées sur la carte (trois au plus), celle de devant en premier.
    windows: Vec<Wid>,
    /// Toute la carte (miniature + libellé).
    rect: RECT,
    /// Zone réservée à la miniature.
    thumb_box: RECT,
    /// Emplacement de la miniature à plat (proportions de la fenêtre), avant l'inclinaison.
    dest: RECT,
    icon: HICON,
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
    hwnd: HWND,
    overlay: HWND,
    model: Model,
    enabled: bool,
    guard_until: Instant,
    /// Fenêtres qu'on vient de réduire : leur événement de réduction vient de nous.
    pending_minimize: HashSet<Wid>,
    /// Position de chaque fenêtre au moment où on l'a réduite : c'est là qu'elle reviendra.
    /// (Pour une fenêtre ancrée sur un bord, Windows ne donne que sa position d'avant l'ancrage.)
    last_rects: HashMap<Wid, RECT>,
    /// Peut-on piloter les fenêtres de ce processus ? (pas s'il est admin et nous non)
    manageable: HashMap<u32, bool>,
    self_elevated: bool,
    cards: Vec<Card>,
    /// Image de chaque fenêtre au moment où elle a quitté l'écran (cartes inclinées).
    snapshots: HashMap<Wid, Rc<Image>>,
    /// Carte de la scène qui revient à l'écran : elle s'efface pendant que ses fenêtres partent.
    departing: Option<Card>,
    layout_key: LayoutKey,
    icons: HashMap<Wid, HICON>,
    hover: Option<u64>,
    tracking_mouse: bool,
    font: HFONT,
    font_bold: HFONT,
    font_icons: HFONT,
    media: Media,
    /// Morceau en cours, affiché dans le lecteur en bas de la barre.
    now_playing: Option<NowPlaying>,
    hover_play: bool,
    /// Fond d'écran flouté derrière la barre, vu à travers le verre du lecteur.
    backdrop: Option<Rc<Image>>,
    /// Dépôt du fil qui charge le fond d'écran.
    backdrop_loaded: Arc<Mutex<Option<Image>>>,
    appbar: Option<RECT>,
    monitor: RECT,
    taskbar_created: u32,
    anim_duration: Duration,
    switch: Option<Switch>,
    slide_start: Option<Instant>,
    arriving: Option<u64>,
    drag: Option<Drag>,
    /// Point de départ imposé au prochain changement de scène (la carte qu'on vient de lâcher).
    drag_source: Option<RECT>,
    /// Le Dock, en bas de l'écran.
    dock: DockUi,
    /// Fenêtre de la scène active qu'on déplace par sa barre de titre : la lâcher sur la barre
    /// la sort de sa scène.
    window_drag: Option<WindowDrag>,
    /// La souris survole la barre pendant ce déplacement : la barre s'éclaire.
    drop_target: bool,
    /// Positions à rendre aux fenêtres détachées une fois réduites (voir `detach`).
    pending_placements: Vec<WindowDrag>,
}

/// Fenêtre déplacée par l'utilisateur, avec sa position d'avant le déplacement.
#[derive(Clone, Copy)]
struct WindowDrag {
    window: Wid,
    rect: RECT,
    maximized: bool,
}

impl App {
    fn new(hwnd: HWND, overlay: HWND, dock: HWND) -> Self {
        let anim_ms = std::env::var("STAGE_MANAGER_ANIM_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(ANIM_MS);
        Self {
            hwnd,
            overlay,
            model: Model::default(),
            enabled: false,
            guard_until: Instant::now(),
            pending_minimize: HashSet::new(),
            last_rects: HashMap::new(),
            manageable: HashMap::new(),
            self_elevated: win::is_elevated(None).unwrap_or(false),
            cards: Vec::new(),
            snapshots: HashMap::new(),
            departing: None,
            layout_key: Vec::new(),
            icons: HashMap::new(),
            hover: None,
            tracking_mouse: false,
            font: HFONT::default(),
            font_bold: HFONT::default(),
            font_icons: HFONT::default(),
            media: Media::start(hwnd, WM_MEDIA),
            now_playing: None,
            hover_play: false,
            backdrop: None,
            backdrop_loaded: Arc::new(Mutex::new(None)),
            appbar: None,
            monitor: RECT::default(),
            taskbar_created: unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) },
            anim_duration: Duration::from_millis(anim_ms),
            switch: None,
            slide_start: None,
            arriving: None,
            drag: None,
            drag_source: None,
            dock: DockUi::new(dock),
            window_drag: None,
            drop_target: false,
            pending_placements: Vec::new(),
        }
    }

    fn dpi(&self) -> i32 {
        match unsafe { GetDpiForWindow(self.hwnd) } {
            0 => 96,
            dpi => dpi as i32,
        }
    }

    fn handle(&mut self, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
        if self.handle_dock_message(msg, wp, lp) {
            return Some(LRESULT(0));
        }
        match msg {
            WM_WINEVENT => self.on_event(wp.0 as u32, HWND(lp.0 as _)),
            WM_MEDIA => {
                let had_player = self.now_playing.is_some();
                self.now_playing = self.media.now_playing();
                if had_player != self.now_playing.is_some() {
                    // Le lecteur apparaît ou disparaît : la place des cartes change.
                    self.layout_key.clear();
                    self.refresh();
                }
                self.render();
            }
            WM_WALLPAPER => {
                let loaded = self.backdrop_loaded.lock().ok().and_then(|mut l| l.take());
                if let Some(image) = loaded {
                    self.backdrop = Some(Rc::new(image));
                    self.render();
                }
            }
            WM_SETTINGCHANGE if wp.0 == SPI_SETDESKWALLPAPER => {
                self.load_backdrop();
                self.reload_dock_backdrop();
            }
            WM_TIMER if wp.0 == TIMER_RESCAN => self.rescan(),
            WM_TIMER if wp.0 == TIMER_WINDOW_DRAG => self.set_drop_target(self.window_over_bar()),
            WM_MOUSEMOVE if self.drag.is_some() => self.drag_to(point(lp)),
            WM_MOUSEMOVE => self.on_mouse_move(point(lp)),
            WM_MOUSELEAVE => {
                self.tracking_mouse = false;
                self.hover_play = false;
                self.set_hover(None);
            }
            WM_LBUTTONDOWN => self.press(point(lp)),
            WM_LBUTTONUP => self.release(point(lp)),
            // Capture perdue (Alt+Tab, Échap…) : on annule le glisser.
            WM_CAPTURECHANGED => self.cancel_drag(),
            WM_HOTKEY if wp.0 == HOTKEY_TOGGLE as usize => self.toggle(),
            WM_HOTKEY if wp.0 == HOTKEY_DETACH as usize => {
                let foreground = wid_of(unsafe { GetForegroundWindow() });
                self.detach(foreground, None);
            }
            WM_APPBAR => match wp.0 as u32 {
                ABN_POSCHANGED => self.place_appbar(),
                ABN_FULLSCREENAPP if self.enabled => unsafe {
                    let cmd = if lp.0 != 0 { SW_HIDE } else { SW_SHOWNOACTIVATE };
                    let _ = ShowWindow(self.hwnd, cmd);
                },
                _ => {}
            },
            WM_DISPLAYCHANGE | WM_DPICHANGED => self.place_appbar(),
            WM_ENDSESSION if wp.0 != 0 => restore_hidden(),
            WM_CLOSE => {}
            m if m == self.taskbar_created => {
                // L'explorateur a redémarré : il a perdu notre icône et notre barre.
                self.add_tray();
                if self.appbar.take().is_some() {
                    self.register_appbar();
                }
                self.reregister_dock();
            }
            _ => return None,
        }
        Some(LRESULT(0))
    }

    // --- Activation ---------------------------------------------------------------

    fn toggle(&mut self) {
        if self.enabled {
            self.disable();
        } else {
            self.enable();
        }
    }

    fn enable(&mut self) {
        if self.enabled {
            return;
        }
        self.enabled = true;
        self.register_appbar();
        self.enable_dock();
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
        }
        let windows: Vec<(Wid, String)> = win::top_level_windows()
            .into_iter()
            .filter(|h| !win::is_minimized(*h))
            .filter_map(|h| self.app_key(h).map(|app| (wid_of(h), app)))
            .collect();
        let foreground = unsafe { GetForegroundWindow() };
        let t = self.model.init(&windows, Some(wid_of(foreground)));
        self.apply(t, false);
        self.refresh();
        unsafe {
            SetTimer(self.hwnd, TIMER_RESCAN, 1500, None);
        }
    }

    fn disable(&mut self) {
        if !self.enabled {
            return;
        }
        self.enabled = false;
        unsafe {
            let _ = KillTimer(self.hwnd, TIMER_RESCAN);
            let _ = KillTimer(self.hwnd, TIMER_WINDOW_DRAG);
        }
        self.cancel_drag();
        self.window_drag = None;
        self.drop_target = false;
        // Termine l'animation (et replace les fenêtres détachées) avant de tout restaurer.
        self.finish_switch();
        self.slide_start = None;
        restore_hidden();
        self.model = Model::default();
        self.pending_minimize.clear();
        self.icons.clear();
        self.last_rects.clear();
        self.cards.clear();
        self.snapshots.clear();
        self.departing = None;
        self.layout_key.clear();
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
        self.remove_appbar();
        self.disable_dock();
    }

    // --- Scènes ---------------------------------------------------------------------

    /// Clé d'application d'une fenêtre, ou `None` si on ne doit pas la gérer.
    fn app_key(&mut self, h: HWND) -> Option<String> {
        if !win::is_app_window(h) {
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
        if !self.enabled {
            return;
        }
        let w = wid_of(h);
        let guarded = Instant::now() < self.guard_until;
        match event {
            EVENT_SYSTEM_FOREGROUND => {
                if guarded && !self.model.in_active_stage(w) {
                    return;
                }
                let t = if let Some(id) = self.model.stage_of(w) {
                    self.model.activate(id, Some(w))
                } else if win::is_minimized(h) {
                    return;
                } else {
                    let Some(app) = self.app_key(h) else { return };
                    self.model.add_focused(w, &app)
                };
                self.apply(t, true);
            }
            EVENT_SYSTEM_MINIMIZESTART => {
                if self.pending_minimize.remove(&w) || !self.model.in_active_stage(w) {
                    return;
                }
                // Réduite par l'utilisateur : elle quitte sa scène, comme sur macOS.
                self.forget(w);
            }
            EVENT_SYSTEM_MINIMIZEEND => {
                if guarded || self.model.in_active_stage(w) {
                    return;
                }
                let Some(id) = self.model.stage_of(w) else { return };
                let t = self.model.activate(id, Some(w));
                self.apply(t, true);
            }
            EVENT_OBJECT_HIDE | EVENT_OBJECT_DESTROY => {
                if !self.model.is_tracked(w) || win::is_alive(h) {
                    return;
                }
                self.forget(w);
            }
            EVENT_SYSTEM_MOVESIZESTART => {
                if !self.model.can_detach(w) {
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
                let over = self.window_over_bar();
                let Some(drag) = self.window_drag.take() else { return };
                unsafe {
                    let _ = KillTimer(self.hwnd, TIMER_WINDOW_DRAG);
                }
                self.set_drop_target(false);
                if !over {
                    return;
                }
                self.detach(w, Some(drag));
            }
            _ => return,
        }
        self.refresh();
        // Une appli a pu s'ouvrir ou se fermer : le Dock suit.
        self.update_running();
    }

    /// Sort une fenêtre de la scène active : elle rejoint la barre comme scène à part.
    /// `moved` : si on l'a traînée jusqu'à la barre, sa position d'avant le déplacement,
    /// pour qu'elle revienne là et non à l'endroit où on l'a lâchée.
    fn detach(&mut self, w: Wid, moved: Option<WindowDrag>) {
        let t = self.model.detach(w);
        if t.is_empty() {
            return;
        }
        self.apply(t, true);
        if let Some(drag) = moved {
            self.last_rects.insert(w, drag.rect);
            self.pending_placements.push(drag);
            if self.switch.is_none() {
                self.apply_placements();
            }
        }
        self.refresh();
    }

    /// À appeler une fois les fenêtres réduites : changer leur position ne se voit plus.
    fn apply_placements(&mut self) {
        for drag in std::mem::take(&mut self.pending_placements) {
            anim::set_restore_position(hwnd_of(drag.window), drag.rect, drag.maximized);
        }
    }

    /// La fenêtre en cours de déplacement est-elle lâchable sur la barre ?
    fn window_over_bar(&self) -> bool {
        let Some(drag) = self.window_drag else { return false };
        // Un redimensionnement passe par les mêmes événements : on ne retient que les
        // déplacements (taille inchangée). Une fenêtre agrandie reprend sa taille normale
        // dès qu'on la tire, ce n'est pas un redimensionnement.
        let resized = size(anim::window_rect(hwnd_of(drag.window))) != size(drag.rect);
        if resized && !drag.maximized {
            return false;
        }
        let mut pt = POINT::default();
        unsafe { GetCursorPos(&mut pt) }.is_ok() && self.appbar.is_some_and(|bar| contains(bar, pt))
    }

    fn set_drop_target(&mut self, over: bool) {
        if self.drop_target != over {
            self.drop_target = over;
            self.render();
        }
    }

    fn forget(&mut self, w: Wid) {
        self.model.remove(w);
        self.pending_minimize.remove(&w);
        self.icons.remove(&w);
        self.last_rects.remove(&w);
        self.snapshots.remove(&w);
        HIDDEN.lock().unwrap_or_else(|e| e.into_inner()).retain(|x| *x != w);
    }

    /// Filet de sécurité : purge les fenêtres disparues et adopte celles apparues sans focus.
    fn rescan(&mut self) {
        if !self.enabled {
            return;
        }
        for w in self.model.windows() {
            if !win::is_alive(hwnd_of(w)) {
                self.forget(w);
            }
        }
        if Instant::now() >= self.guard_until {
            for h in win::top_level_windows() {
                let w = wid_of(h);
                if self.model.is_tracked(w) || win::is_minimized(h) {
                    continue;
                }
                if let Some(app) = self.app_key(h) {
                    self.model.adopt(w, &app);
                }
            }
        }
        self.refresh();
        // Les titres des fenêtres ont pu changer.
        self.invalidate();
        self.update_running();
    }

    fn show_stage(&mut self, stage: u64, merge: bool) {
        let t = if merge { self.model.merge_into_active(stage) } else { self.model.activate(stage, None) };
        self.apply(t, true);
        self.refresh();
    }

    fn apply(&mut self, t: Transition, animate: bool) {
        if t.is_empty() {
            return;
        }
        self.finish_switch();
        // Dernière occasion de photographier les fenêtres sortantes : réduites, elles ne se
        // dessinent plus. Ces images servent aux cartes inclinées de la barre.
        for &w in &t.minimize {
            let h = hwnd_of(w);
            if !win::is_minimized(h) {
                if let Some(image) = snapshot::capture(h, SNAPSHOT_WIDTH) {
                    self.snapshots.insert(w, Rc::new(image));
                }
            }
        }
        {
            let mut hidden = HIDDEN.lock().unwrap_or_else(|e| e.into_inner());
            hidden.retain(|w| !t.restore.contains(w) && t.focus != Some(*w));
            for &w in &t.minimize {
                if !hidden.contains(&w) {
                    hidden.push(w);
                }
            }
        }
        let animate = animate && !self.anim_duration.is_zero() && self.appbar.is_some();
        let extra = if animate { self.anim_duration } else { Duration::ZERO };
        self.guard_until = Instant::now() + SWITCH_GUARD + extra;
        if animate && self.start_switch(&t) {
            return;
        }
        self.reveal(&t.restore, t.focus);
        self.hide(&t.minimize);
    }

    /// Lance l'animation d'un changement de scène. Renvoie `false` s'il n'y a rien à animer.
    fn start_switch(&mut self, t: &Transition) -> bool {
        let now = Instant::now();
        let mon = self.monitor;
        let bar = self.appbar.unwrap_or_default();
        let to_overlay = |r: RECT| offset(r, -mon.left, -mon.top);
        let bar_to_overlay = |r: RECT| offset(r, bar.left - mon.left, bar.top - mon.top);

        // Les fenêtres entrantes partent de leur carte actuelle dans la barre.
        let incoming: Vec<Wid> = t.restore.iter().copied().chain(t.focus).collect();
        // Si on vient de lâcher une carte, elles partent de là où on l'a lâchée.
        let dropped = self.drag_source.take();
        let card = self.cards.iter().find(|c| incoming.contains(&c.window)).map(|c| {
            // Figée à sa position affichée : elle va s'effacer pendant que ses fenêtres partent.
            let (dx, dy) = self.card_offset(c, now);
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
        self.departing = if dropped.is_none() { card } else { None };

        // La scène sortante prend la première place de la barre ; sa carte reste cachée
        // jusqu'à ce que les miniatures l'aient rejointe.
        self.arriving = t.minimize.first().and_then(|w| self.model.stage_of(*w));
        self.refresh();
        let bar_w = bar.right - bar.left;
        let target = self
            .arriving
            .and_then(|id| self.cards.iter().find(|c| c.stage == id))
            .map(|c| bar_to_overlay(c.thumb_box))
            .unwrap_or_else(|| bar_to_overlay(RECT { left: 0, top: 0, right: bar_w, bottom: bar_w * 5 / 8 }));

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
        let mut fly = |w: Wid, from: RECT, to: RECT, fade_in: bool| unsafe {
            if let Ok(thumb) = DwmRegisterThumbnail(self.overlay, hwnd_of(w)) {
                anim::place_thumb(thumb, from, true, if fade_in { 0 } else { 255 });
                flights.push(Flight { thumb, from, to, fade_in });
            }
        };
        // Seules les fenêtres visibles volent, enregistrées de l'arrière vers l'avant
        // pour que la fenêtre de devant reste dessus.
        for (w, from) in anim::visible_layers(&outgoing).into_iter().rev() {
            fly(w, from, fit(target, size(from)), false);
        }
        for (w, to) in anim::visible_layers(&arriving).into_iter().rev() {
            let from = match source {
                Some(card) => fit(card, size(to)),
                None => inflate(to, -(to.right - to.left) / 10),
            };
            fly(w, from, to, true);
        }
        if flights.is_empty() {
            self.departing = None;
            self.reveal_arriving();
            return false;
        }

        for &w in &quiet {
            anim::set_transitions(hwnd_of(w), false);
        }
        // Les miniatures doivent être à l'écran avant que les vraies fenêtres disparaissent.
        self.show_overlay();
        self.hide(&t.minimize);
        self.switch = Some(Switch {
            start: now,
            duration: self.anim_duration,
            flights,
            restore: t.restore.clone(),
            focus: t.focus,
            quiet,
            landed: None,
        });
        true
    }

    fn show_overlay(&self) {
        let mon = self.monitor;
        unsafe {
            // Un pixel de moins que l'écran : sinon Windows croit à une appli plein écran
            // et demande à la barre de se cacher.
            let _ = SetWindowPos(
                self.overlay,
                HWND_TOPMOST,
                mon.left,
                mon.top,
                mon.right - mon.left,
                mon.bottom - mon.top - 1,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
            let _ = DwmFlush();
        }
    }

    fn hide_overlay(&self) {
        unsafe {
            let _ = ShowWindow(self.overlay, SW_HIDE);
        }
    }

    fn animating(&self) -> bool {
        self.switch.is_some() || self.slide_start.is_some() || self.dock_animating()
    }

    /// Une image d'animation, appelée à chaque rafraîchissement de l'écran.
    fn tick(&mut self) {
        let now = Instant::now();
        // La barre se redessine tant que ses cartes bougent ou s'enchaînent en fondu.
        let mut redraw = self.slide_start.is_some();
        if self.slide_start.is_some_and(|start| anim::progress(start, self.anim_duration, now) >= 1.0) {
            self.slide_start = None;
        }
        if let Some(switch) = &self.switch {
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
                    redraw |= self.arriving.is_some() || self.departing.is_some();
                    if p >= 1.0 {
                        self.land(now);
                    }
                }
                Some(landed) if now.duration_since(landed) >= SETTLE => self.finish_switch(),
                Some(_) => {}
            }
        }
        if redraw {
            self.render();
        }
        self.dock_tick(now);
    }

    /// Fin du trajet : les vraies fenêtres reprennent la place de leurs miniatures.
    fn land(&mut self, now: Instant) {
        let Some(switch) = self.switch.as_mut().filter(|s| s.landed.is_none()) else { return };
        switch.landed = Some(now);
        let restore = std::mem::take(&mut switch.restore);
        let focus = switch.focus;
        self.reveal(&restore, focus);
        self.departing = None;
        self.reveal_arriving();
    }

    /// Termine tout de suite l'animation en cours, s'il y en a une.
    fn finish_switch(&mut self) {
        self.land(Instant::now());
        let Some(switch) = self.switch.take() else { return };
        unsafe {
            for f in switch.flights {
                let _ = DwmUnregisterThumbnail(f.thumb);
            }
        }
        // L'overlay porte aussi la carte qu'on est en train de déplacer.
        if !self.dragging() {
            self.hide_overlay();
        }
        self.apply_placements();
        for w in switch.quiet {
            anim::set_transitions(hwnd_of(w), true);
        }
    }

    fn reveal_arriving(&mut self) {
        if self.arriving.take().is_some() {
            self.render();
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

    // --- Barre latérale -------------------------------------------------------------

    fn register_appbar(&mut self) {
        if self.appbar.is_some() {
            return;
        }
        let mut abd = APPBARDATA {
            cbSize: size_of::<APPBARDATA>() as u32,
            hWnd: self.hwnd,
            uCallbackMessage: WM_APPBAR,
            ..Default::default()
        };
        unsafe { SHAppBarMessage(ABM_NEW, &mut abd) };
        self.appbar = Some(RECT::default());
        self.place_appbar();
    }

    fn remove_appbar(&mut self) {
        if self.appbar.take().is_none() {
            return;
        }
        let mut abd = APPBARDATA { cbSize: size_of::<APPBARDATA>() as u32, hWnd: self.hwnd, ..Default::default() };
        unsafe { SHAppBarMessage(ABM_REMOVE, &mut abd) };
    }

    /// Réserve une bande sur le bord gauche de l'écran principal, comme la barre des tâches :
    /// les fenêtres maximisées s'arrêtent à côté.
    fn place_appbar(&mut self) {
        let Some(current) = self.appbar else { return };
        unsafe {
            let monitor = MonitorFromPoint(POINT::default(), MONITOR_DEFAULTTOPRIMARY);
            let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
            let _ = GetMonitorInfoW(monitor, &mut info);
            self.monitor = info.rcMonitor;
            let (mut dpi, mut dpi_y) = (96u32, 96u32);
            let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi, &mut dpi_y);
            let width = BAR_WIDTH * dpi as i32 / 96;

            let mut abd = APPBARDATA {
                cbSize: size_of::<APPBARDATA>() as u32,
                hWnd: self.hwnd,
                uEdge: ABE_LEFT,
                rc: info.rcMonitor,
                ..Default::default()
            };
            abd.rc.right = abd.rc.left + width;
            SHAppBarMessage(ABM_QUERYPOS, &mut abd);
            abd.rc.right = abd.rc.left + width;
            if abd.rc == current {
                return;
            }
            SHAppBarMessage(ABM_SETPOS, &mut abd);
            let r = abd.rc;
            self.appbar = Some(r);
            let _ = SetWindowPos(self.hwnd, HWND_TOPMOST, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOACTIVATE);

            for font in [self.font, self.font_bold, self.font_icons] {
                if !font.is_invalid() {
                    let _ = DeleteObject(font);
                }
            }
            let dpi = self.dpi();
            self.font = make_font("Segoe UI", 12, 400, dpi);
            self.font_bold = make_font("Segoe UI", 12, 600, dpi);
            // Icônes de Windows 11 (lecture, pause).
            self.font_icons = make_font("Segoe Fluent Icons", 14, 400, dpi);
        }
        // La barre a bougé ou changé de taille : le fond derrière elle aussi.
        self.load_backdrop();
        self.layout_key.clear();
        self.refresh();
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

    fn icon(&mut self, w: Wid) -> HICON {
        *self.icons.entry(w).or_insert_with(|| win::icon(hwnd_of(w)))
    }

    /// Recalcule la disposition des cartes de la barre.
    fn refresh(&mut self) {
        let mut rc = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut rc);
        }
        let dpi = self.dpi();
        let s = move |v: i32| v * dpi / 96;
        let margin = s(16);
        let stack = s(10);
        let width = (rc.right - 2 * margin).max(1);
        let thumb_h = (width - stack) * 5 / 8;
        let label_h = s(26);
        let card_h = stack + thumb_h + label_h;

        // Les cartes s'arrêtent au-dessus du lecteur de musique.
        let limit = self.player_layout().map_or(rc.bottom - margin, |p| p.panel.top - s(12));
        let mut layout: LayoutKey = Vec::new();
        let mut y = s(20);
        for stage in self.model.sidebar() {
            if y + card_h > limit {
                break;
            }
            let rect = RECT { left: margin, top: y, right: margin + width, bottom: y + card_h };
            layout.push((stage.id, stage.windows[0], stage.windows.len(), rect));
            y += card_h + s(18);
        }
        if layout == self.layout_key {
            return;
        }

        // Les cartes qui restent glissent depuis l'endroit où elles sont affichées en ce moment.
        let now = Instant::now();
        let previous: HashMap<u64, RECT> = self
            .cards
            .iter()
            .map(|c| {
                let (dx, dy) = self.card_offset(c, now);
                (c.stage, offset(c.rect, dx, dy))
            })
            .collect();
        self.cards.clear();
        let mut sliding = false;
        for &(stage, window, _, rect) in &layout {
            let thumb_box = RECT { left: rect.left, top: rect.top + stack, right: rect.right, bottom: rect.top + stack + thumb_h };
            let windows: Vec<Wid> = self.model.stage(stage).map_or(vec![window], |s| s.windows.iter().take(3).copied().collect());
            let dest = fit(thumb_box, self.window_size(window));
            let slide = previous.get(&stage).map_or((0, 0), |r| (r.left - rect.left, r.top - rect.top));
            // Une carte qui n'était pas affichée (et ne vient pas d'un trajet) apparaît en fondu.
            let fade_in = !previous.contains_key(&stage) && self.arriving != Some(stage);
            sliding |= slide != (0, 0) || fade_in;
            let icon = self.icon(window);
            self.cards.push(Card { stage, window, windows, rect, thumb_box, dest, icon, slide, hidden: false, fade_in });
        }
        self.slide_start = sliding.then_some(now);
        self.layout_key = layout;
        if self.hover.is_some_and(|h| !self.cards.iter().any(|c| c.stage == h)) {
            self.hover = None;
        }
        self.place_cards(now);
        self.invalidate();
    }

    /// Décalage d'affichage d'une carte pendant l'animation de glissement.
    fn card_offset(&self, card: &Card, now: Instant) -> (i32, i32) {
        let remaining = 1.0 - anim::spring(self.slide_progress(now));
        ((card.slide.0 as f64 * remaining) as i32, (card.slide.1 as f64 * remaining) as i32)
    }

    fn slide_progress(&self, now: Instant) -> f64 {
        self.slide_start.map_or(1.0, |start| anim::progress(start, self.anim_duration, now))
    }

    fn place_cards(&self, _now: Instant) {
        self.render();
    }

    /// Taille de la fenêtre telle qu'on la dessine : son instantané, sinon sa position mémorisée.
    fn window_size(&self, w: Wid) -> SIZE {
        if let Some(img) = self.snapshots.get(&w) {
            return SIZE { cx: img.w, cy: img.h };
        }
        let rect = self.last_rects.get(&w).copied().unwrap_or_else(|| anim::restored_rect(hwnd_of(w)));
        size(rect)
    }

    /// Opacité d'une carte : fondu d'apparition, fondu enchaîné avec les miniatures qui la
    /// rejoignent, ou invisible pendant qu'on la déplace.
    fn card_alpha(&self, card: &Card, now: Instant) -> u8 {
        if self.arriving == Some(card.stage) {
            let p = self.switch.as_ref().map_or(1.0, |s| anim::progress(s.start, s.duration, now));
            return (anim::cross_fade(p) * 255.0) as u8;
        }
        if card.hidden {
            return 0;
        }
        if card.fade_in {
            return anim::fade_in(self.slide_progress(now));
        }
        255
    }

    fn invalidate(&self) {
        self.render();
    }

    fn hit(&self, pt: POINT) -> Option<u64> {
        self.cards.iter().find(|c| contains(c.rect, pt)).map(|c| c.stage)
    }

    fn on_mouse_move(&mut self, pt: POINT) {
        if !self.tracking_mouse {
            let mut tme = TRACKMOUSEEVENT {
                cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: self.hwnd,
                dwHoverTime: 0,
            };
            self.tracking_mouse = unsafe { TrackMouseEvent(&mut tme) }.is_ok();
        }
        let over_play = self.player_layout().is_some_and(|p| contains(p.button, pt));
        if over_play != self.hover_play {
            self.hover_play = over_play;
            self.render();
        }
        self.set_hover(self.hit(pt));
    }

    fn load_backdrop(&self) {
        if let Some(bar) = self.appbar {
            let blur = 10 * self.dpi() / 96;
            wallpaper::load_async(bar, self.monitor, blur, self.hwnd, WM_WALLPAPER, Arc::clone(&self.backdrop_loaded));
        }
    }

    /// Lecteur de musique en bas de la barre, s'il y a un morceau en cours.
    fn player_layout(&self) -> Option<PlayerLayout> {
        self.now_playing.as_ref()?;
        let bar = self.appbar?;
        let (w, h) = (bar.right - bar.left, bar.bottom - bar.top);
        let dpi = self.dpi();
        let s = move |v: i32| v * dpi / 96;
        let (pad, art) = (s(10), s(56));
        let bottom = h - s(14);
        let top = bottom - (2 * pad + art + s(8) + s(32));
        let panel = RECT { left: s(10), top, right: w - s(10), bottom };
        let art_rect = RECT { left: panel.left + pad, top: top + pad, right: panel.left + pad + art, bottom: top + pad + art };
        let text_left = art_rect.right + s(10);
        let title = RECT { left: text_left, top: art_rect.top + s(6), right: panel.right - pad, bottom: art_rect.top + s(26) };
        let artist = RECT { top: title.bottom, bottom: title.bottom + s(20), ..title };
        let size = s(32);
        let cx = (panel.left + panel.right) / 2;
        let button = RECT { left: cx - size / 2, top: art_rect.bottom + s(8), right: cx + size / 2, bottom: art_rect.bottom + s(8) + size };
        Some(PlayerLayout { panel, art: art_rect, title, artist, button })
    }

    fn set_hover(&mut self, hover: Option<u64>) {
        if self.hover != hover {
            self.hover = hover;
            self.invalidate();
        }
    }

    // --- Glisser-déposer ------------------------------------------------------------

    fn dragging(&self) -> bool {
        self.drag.as_ref().is_some_and(|d| d.ghost.is_some())
    }

    fn press(&mut self, pt: POINT) {
        if self.player_layout().is_some_and(|p| contains(p.button, pt)) {
            self.media.toggle_play_pause();
            // Affiché tout de suite, sans attendre la confirmation du lecteur.
            if let Some(np) = self.now_playing.as_mut() {
                np.playing = !np.playing;
            }
            self.render();
            return;
        }
        let Some(bar) = self.appbar else { return };
        let Some(card) = self.cards.iter().find(|c| !c.hidden && contains(c.rect, pt)) else { return };
        let mon = self.monitor;
        let (dx, dy) = self.card_offset(card, Instant::now());
        let origin = offset(card.dest, dx + bar.left - mon.left, dy + bar.top - mon.top);
        self.drag = Some(Drag { stage: card.stage, window: card.window, start: pt, origin, ghost: None, rect: origin });
        unsafe {
            // Garde la souris même quand elle sort de la barre.
            SetCapture(self.hwnd);
        }
    }

    fn drag_to(&mut self, pt: POINT) {
        let Some(bar) = self.appbar else { return };
        let Some(start) = self.drag.as_ref().map(|d| d.start) else { return };
        if !self.dragging() {
            // Un petit tremblement pendant un clic ne doit pas lancer un glisser.
            let threshold = unsafe { GetSystemMetrics(SM_CXDRAG) }.max(4);
            if (pt.x - start.x).abs() < threshold && (pt.y - start.y).abs() < threshold {
                return;
            }
            self.begin_drag();
        }
        let mon = self.monitor;
        let Some(drag) = self.drag.as_mut() else { return };
        let Some(ghost) = drag.ghost else { return };
        let to_overlay = |p: POINT| POINT { x: p.x + bar.left - mon.left, y: p.y + bar.top - mon.top };
        let (cursor, grab) = (to_overlay(pt), to_overlay(drag.start));
        // La miniature grandit à mesure qu'elle s'éloigne de la barre.
        let away = (cursor.x - (bar.right - mon.left)).max(0) as f64;
        let scale = 1.0 + (away / 600.0).min(1.0) * 1.5;
        let scaled = |v: i32| (v as f64 * scale) as i32;
        let o = drag.origin;
        let left = cursor.x - scaled(grab.x - o.left);
        let top = cursor.y - scaled(grab.y - o.top);
        drag.rect = RECT { left, top, right: left + scaled(o.right - o.left), bottom: top + scaled(o.bottom - o.top) };
        anim::place_thumb(ghost, drag.rect, true, 235);
    }

    fn begin_drag(&mut self) {
        self.finish_switch();
        let overlay = self.overlay;
        let Some(drag) = self.drag.as_mut() else { return };
        let Ok(ghost) = (unsafe { DwmRegisterThumbnail(overlay, hwnd_of(drag.window)) }) else { return };
        anim::place_thumb(ghost, drag.origin, true, 235);
        drag.ghost = Some(ghost);
        let stage = drag.stage;
        // La carte quitte la barre pendant qu'on la déplace.
        self.set_card_hidden(stage, true);
        self.hover = None;
        self.show_overlay();
        self.render();
    }

    fn release(&mut self, pt: POINT) {
        let Some(drag) = self.drag.take() else { return };
        unsafe {
            let _ = ReleaseCapture();
        }
        let Some(ghost) = drag.ghost else {
            // Simple clic : Maj ajoute la scène à la scène active.
            if self.hit(pt) == Some(drag.stage) {
                let merge = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
                self.show_stage(drag.stage, merge);
            }
            return;
        };
        let bar_w = self.appbar.map_or(0, |b| b.right - b.left);
        if pt.x >= bar_w {
            // Lâchée hors de la barre : la scène rejoint la scène active, en partant d'ici.
            self.drag_source = Some(drag.rect);
            self.show_stage(drag.stage, true);
            self.drag_source = None;
        }
        self.drop_ghost(ghost, drag.stage);
    }

    fn cancel_drag(&mut self) {
        let Some(drag) = self.drag.take() else { return };
        if let Some(ghost) = drag.ghost {
            self.drop_ghost(ghost, drag.stage);
        }
    }

    fn drop_ghost(&mut self, ghost: isize, stage: u64) {
        unsafe {
            let _ = DwmUnregisterThumbnail(ghost);
        }
        if self.switch.is_none() {
            self.hide_overlay();
        }
        // Lâchée sur la barre : la carte reprend sa place.
        self.set_card_hidden(stage, false);
        self.render();
    }

    fn set_card_hidden(&mut self, stage: u64, hidden: bool) {
        for card in self.cards.iter_mut().filter(|c| c.stage == stage) {
            card.hidden = hidden;
        }
        self.place_cards(Instant::now());
    }

    /// Redessine la barre. Fond transparent : seuls les cartes, le survol et les libellés
    /// sont visibles, posés directement sur le fond d'écran.
    fn render(&self) {
        let Some(bar) = self.appbar.filter(|_| self.enabled) else { return };
        let (w, h) = (bar.right - bar.left, bar.bottom - bar.top);
        let dpi = self.dpi();
        let s = move |v: i32| v * dpi / 96;
        let now = Instant::now();
        // (carte, rectangle, miniature à plat, décalage vertical, opacité)
        let mut cards: Vec<(&Card, RECT, RECT, i32, u8)> = self
            .cards
            .iter()
            .map(|c| {
                let (dx, dy) = self.card_offset(c, now);
                (c, offset(c.rect, dx, dy), offset(c.dest, dx, dy), dy, self.card_alpha(c, now))
            })
            .filter(|c| c.4 > 0)
            .collect();
        // La carte de la scène qui revient à l'écran s'efface pendant que ses fenêtres partent.
        if let (Some(card), Some(switch)) = (&self.departing, &self.switch) {
            let alpha = 255 - anim::fade_in(anim::progress(switch.start, switch.duration, now));
            if alpha > 0 {
                cards.push((card, card.rect, card.dest, 0, alpha));
            }
        }

        let mut canvas = Canvas::new(w, h);
        // Alpha 1 et non 0 : invisible, mais la barre reste cliquable partout
        // (Windows laisse passer les clics sur les pixels totalement transparents).
        canvas.clear((0, 0, 0), 1);
        let white = (255, 255, 255);
        if self.drop_target {
            // Une fenêtre est traînée au-dessus de la barre : la lâcher la sortira de sa scène.
            canvas.round_rect(RECT { left: s(6), top: s(6), right: w - s(6), bottom: h - s(6) }, s(18), white, 56);
        }
        let solid = |r: u8, g: u8, b: u8| Image { w: 1, h: 1, px: vec![0xFF00_0000 | (r as u32) << 16 | (g as u32) << 8 | b as u32] };
        let (shadow, placeholder) = (solid(0, 0, 0), solid(58, 58, 66));
        for &(card, rect, dest, _, alpha) in &cards {
            let fade = |a: u8| (a as u32 * alpha as u32 / 255) as u8;
            if self.hover == Some(card.stage) {
                canvas.round_rect(inflate(rect, s(8)), s(16), white, fade(46));
            }
            // Fenêtres de la scène empilées derrière, de la plus lointaine à la plus proche.
            for (depth, &window) in card.windows.iter().enumerate().skip(1).rev() {
                let d = depth as i32;
                let image = self.snapshots.get(&window);
                let area = offset(inflate(dest, -s(6) * d), 0, -s(9) * d);
                let r = image.map_or(area, |img| fit(area, SIZE { cx: img.w, cy: img.h }));
                let img = image.map_or(&placeholder, |i| i.as_ref());
                canvas.draw_quad(img, tilt(r), alpha, (50 * d) as u8);
            }
            // Ombre douce sous la miniature, puis la miniature elle-même.
            canvas.draw_quad(&shadow, tilt(offset(inflate(dest, s(2)), s(1), s(4))), fade(40), 0);
            canvas.draw_quad(&shadow, tilt(offset(dest, s(1), s(2))), fade(70), 0);
            let image = self.snapshots.get(&card.window);
            canvas.draw_quad(image.map_or(&placeholder, |i| i.as_ref()), tilt(dest), alpha, 0);
        }
        // Lecteur de musique : plaque de verre liquide, pochette et bulle lecture/pause.
        let player = self.player_layout().zip(self.now_playing.as_ref());
        if let Some((p, np)) = &player {
            let flat = |r: RECT| {
                let (l, t, rr, b) = (r.left as f32, r.top as f32, r.right as f32, r.bottom as f32);
                [(l, t), (rr, t), (rr, b), (l, b)]
            };
            let sf = |v: i32| s(v) as f32;
            // Ombre douce sous la plaque.
            canvas.round_rect(offset(inflate(p.panel, s(2)), 0, s(5)), s(22), (0, 0, 0), 45);
            canvas.round_rect(offset(p.panel, 0, s(2)), s(20), (0, 0, 0), 55);
            let panel_glass = Glass { frost: 0.14, darken: 0.06, refraction: sf(14), bevel: sf(18), rim: 0.85 };
            match &self.backdrop {
                Some(backdrop) => canvas.glass(p.panel, sf(20), backdrop, (0, 0), &panel_glass),
                // Fond d'écran pas encore chargé : simple panneau sombre translucide.
                None => canvas.round_rect(p.panel, s(20), (18, 18, 22), 150),
            }
            canvas.round_rect(offset(p.art, 0, s(2)), s(8), (0, 0, 0), 70);
            match &np.art {
                Some(art) => canvas.draw_quad(art, flat(p.art), 255, 0),
                None => canvas.round_rect(p.art, s(6), (70, 70, 80), 255),
            }
            // Bulle de verre plus claire pour le bouton, encore plus claire au survol.
            let radius = (p.button.right - p.button.left) as f32 / 2.0;
            let frost = if self.hover_play { 0.5 } else { 0.34 };
            let bubble = Glass { frost, darken: 0.05, refraction: sf(6), bevel: sf(9), rim: 0.9 };
            match &self.backdrop {
                Some(backdrop) => canvas.glass(p.button, radius, backdrop, (0, 0), &bubble),
                None => canvas.round_rect(p.button, radius as i32, white, if self.hover_play { 110 } else { 70 }),
            }
        }
        canvas.gdi(|dc| unsafe {
            SelectObject(dc, self.font);
            SetBkMode(dc, TRANSPARENT);
            // Les libellés n'apparaissent qu'une fois la carte assez visible : GDI ne sait pas
            // dessiner en semi-transparence.
            for &(card, rect, dest, dy, _) in cards.iter().filter(|c| c.4 >= 160) {
                if !self.snapshots.contains_key(&card.window) {
                    // Pas d'image de la fenêtre : son icône au milieu de la carte.
                    let big = s(32);
                    let (cx, cy) = ((dest.left + dest.right - big) / 2, (dest.top + dest.bottom - big) / 2);
                    let _ = DrawIconEx(dc, cx, cy, card.icon, big, big, 0, None, DI_NORMAL);
                }
                let label_top = card.thumb_box.bottom + dy + s(4);
                let icon = s(16);
                let _ = DrawIconEx(
                    dc,
                    rect.left,
                    label_top + (rect.bottom - label_top - icon) / 2,
                    card.icon,
                    icon,
                    icon,
                    0,
                    None,
                    DI_NORMAL,
                );
                let title = win::title(hwnd_of(card.window));
                let text_rect = RECT { left: rect.left + icon + s(6), top: label_top, right: rect.right, bottom: rect.bottom };
                let flags = DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX;
                draw_text(dc, &title, text_rect, flags, TEXT, s(1));
            }
            let centered = DT_CENTER | DT_VCENTER | DT_SINGLELINE;
            if let Some((p, np)) = &player {
                let line = DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX;
                SelectObject(dc, self.font_bold);
                draw_text(dc, &np.title, p.title, line, TEXT, s(1));
                SelectObject(dc, self.font);
                draw_text(dc, &np.artist, p.artist, line, TEXT_DIM, s(1));
                // Icônes Segoe Fluent : E769 pause, E768 lecture.
                SelectObject(dc, self.font_icons);
                let glyph = String::from_utf16_lossy(&[if np.playing { 0xE769 } else { 0xE768 }]);
                draw_text(dc, &glyph, p.button, centered, TEXT, s(1));
                SelectObject(dc, self.font);
            }
            if self.drop_target {
                // Au-dessus du lecteur s'il est affiché.
                let bottom = player.as_ref().map_or(h - s(30), |(p, _)| p.panel.top - s(10));
                let text_rect = RECT { left: 0, top: bottom - s(40), right: w, bottom };
                draw_text(dc, "Lâcher pour séparer", text_rect, centered, TEXT, s(1));
            } else if cards.is_empty() {
                let text_rect = RECT { left: 0, top: s(20), right: w, bottom: s(60) };
                draw_text(dc, "Aucune autre scène", text_rect, centered, TEXT_DIM, s(1));
            }
        });
        canvas.present(self.hwnd, POINT { x: bar.left, y: bar.top });
    }
}

// --- Géométrie et dessin -------------------------------------------------------------

/// Incline une miniature comme sur macOS : la carte pivote, son bord droit (côté écran)
/// s'éloigne et rapetisse.
fn tilt(r: RECT) -> [(f32, f32); 4] {
    let (l, t, rr, b) = (r.left as f32, r.top as f32, r.right as f32, r.bottom as f32);
    let (inset, squeeze) = ((b - t) * 0.11, (rr - l) * 0.07);
    [(l, t), (rr - squeeze, t + inset), (rr - squeeze, b - inset), (l, b)]
}

fn contains(r: RECT, pt: POINT) -> bool {
    pt.x >= r.left && pt.x < r.right && pt.y >= r.top && pt.y < r.bottom
}

fn point(lp: LPARAM) -> POINT {
    POINT { x: (lp.0 & 0xFFFF) as i16 as i32, y: ((lp.0 >> 16) & 0xFFFF) as i16 as i32 }
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

/// Emplacement des éléments du lecteur de musique, en bas de la barre.
struct PlayerLayout {
    panel: RECT,
    art: RECT,
    title: RECT,
    artist: RECT,
    button: RECT,
}

/// Texte avec une ombre portée, pour rester lisible sur n'importe quel fond d'écran.
unsafe fn draw_text(dc: HDC, text: &str, rect: RECT, flags: DRAW_TEXT_FORMAT, color: COLORREF, shadow: i32) {
    let mut text: Vec<u16> = text.encode_utf16().collect();
    let mut shadow_rect = offset(rect, shadow, shadow);
    SetTextColor(dc, rgb(0, 0, 0));
    DrawTextW(dc, &mut text, &mut shadow_rect, flags);
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
    let sidebar = HWND(SIDEBAR.load(Ordering::Relaxed) as _);
    let _ = PostMessageW(sidebar, WM_WINEVENT, WPARAM(event as usize), LPARAM(hwnd.0 as isize));
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let is_dock = hwnd.0 as isize == DOCK.load(Ordering::Relaxed);
    match msg {
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        WM_ERASEBKGND => return LRESULT(1),
        WM_RBUTTONUP if is_dock => {
            dockbar::dock_menu(hwnd, point(lp));
            return LRESULT(0);
        }
        WM_MOUSEMOVE | WM_MOUSELEAVE | WM_LBUTTONDOWN | WM_LBUTTONUP if is_dock => {
            with_app(|app| app.dock_mouse(msg, lp));
            return LRESULT(0);
        }
        // Les menus ont leur propre boucle de messages : on les ouvre hors de `with_app`.
        WM_TRAY => {
            let mouse = (lp.0 & 0xFFFF) as u32;
            if mouse == WM_RBUTTONUP || mouse == WM_CONTEXTMENU {
                tray_menu(hwnd);
            }
            return LRESULT(0);
        }
        WM_RBUTTONUP => {
            if let Some(Some(stage)) = with_app(|app| app.hit(point(lp))) {
                card_menu(hwnd, stage);
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
            if [WM_WINEVENT, WM_APPBAR, WM_MEDIA, WM_WALLPAPER, WM_DOCKBAR, WM_DOCK_LOADED, WM_DOCK_WALLPAPER].contains(&msg) {
                let _ = PostMessageW(hwnd, msg, wp, lp);
                return LRESULT(0);
            }
            DefWindowProcW(hwnd, msg, wp, lp)
        }
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
    let Some(enabled) = with_app(|app| app.enabled) else { return };
    let toggle = if enabled { w!("Désactiver\tCtrl+Alt+S") } else { w!("Activer\tCtrl+Alt+S") };
    let autostart = autostart::is_enabled();
    let items = [
        (MENU_TOGGLE, toggle),
        (MENU_AUTOSTART, w!("Lancer au démarrage de Windows")),
        (0, PCWSTR::null()),
        (MENU_QUIT, w!("Quitter")),
    ];
    let checked = if autostart { vec![MENU_AUTOSTART] } else { vec![] };
    match popup(hwnd, &items, &checked) {
        MENU_TOGGLE => {
            with_app(App::toggle);
        }
        MENU_AUTOSTART => autostart::set(!autostart),
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
                app.refresh();
            }
        }),
        _ => None,
    };
}

pub fn run() -> Result<()> {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        // COM : le Dock lance des applis et ouvre la corbeille via le shell.
        let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
        let instance: HINSTANCE = GetModuleHandleW(None)?.into();
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
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
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
        SIDEBAR.store(hwnd.0 as isize, Ordering::Relaxed);
        let overlay = anim::create_overlay(instance)?;
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
        APP.with(|cell| *cell.borrow_mut() = Some(App::new(hwnd, overlay, dock)));

        let flags = WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS;
        let hooks = [
            SetWinEventHook(EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_MINIMIZEEND, None, Some(win_event), 0, 0, flags),
            SetWinEventHook(EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE, None, Some(win_event), 0, 0, flags),
        ];
        let _ = RegisterHotKey(hwnd, HOTKEY_TOGGLE, MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, 'S' as u32);
        let _ = RegisterHotKey(hwnd, HOTKEY_DETACH, MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, 'D' as u32);
        with_app(|app| {
            app.add_tray();
            app.enable();
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
            app.disable();
            app.remove_tray();
        });
        let _ = DestroyWindow(hwnd);
    }
    Ok(())
}
