//! Le Dock à l'écran : sa fenêtre, sa place réservée en bas de l'écran, son dessin, ses clics.

use std::collections::HashSet;

use super::*;
use crate::{
    canvas::round_corners,
    dock::{self, Dock, Kind, Metrics},
    shell, volume,
};

pub(super) const WM_DOCKBAR: u32 = WM_APP + 6;
pub(super) const WM_DOCK_LOADED: u32 = WM_APP + 7;
pub(super) const WM_DOCK_WALLPAPER: u32 = WM_APP + 8;

const TIMER_CLOCK: usize = 3;

/// Largeur du panneau du volume (à droite du Dock) et son écart avec le Dock, en pixels à 100 %.
const VOLUME_WIDTH: i32 = 54;
const PANEL_GAP: i32 = 10;
/// Hauteur du curseur de volume, vertical au-dessus de son bouton (pixels à 100 %).
const SLIDER_HEIGHT: i32 = 210;
/// Ouverture (avec un léger rebond) et fermeture du curseur.
const SLIDER_OPEN: Duration = Duration::from_millis(320);
const SLIDER_CLOSE: Duration = Duration::from_millis(180);

const MENU_DOCK_OPEN: usize = 20;
const MENU_DOCK_PIN: usize = 21;
const MENU_DOCK_QUIT: usize = 22;
const MENU_TRASH_EMPTY: usize = 23;

pub(super) static DOCK: AtomicIsize = AtomicIsize::new(0);

pub(super) struct DockUi {
    pub hwnd: HWND,
    pub model: Dock,
    /// Bande réservée en bas de l'écran (les fenêtres agrandies s'arrêtent au-dessus).
    reserved: Option<RECT>,
    /// Position de la fenêtre du Dock à l'écran.
    window: RECT,
    dpi: i32,
    loader: shell::IconLoader,
    requested: HashSet<String>,
    /// Fond d'écran flouté derrière le Dock (toute la largeur de l'écran).
    backdrop: Option<Rc<Image>>,
    backdrop_rect: RECT,
    backdrop_loaded: Arc<Mutex<Option<Image>>>,
    tracking: bool,
    trash_full: bool,
    /// Bouton du panneau du volume sous la souris.
    hover_control: Option<Control>,
    /// Heure et date, pour redessiner l'heure de l'encoche au changement de minute.
    clock: (String, String),
    /// Volume (entre 0 et 1) et son coupé, relus chaque seconde.
    volume: Option<(f32, bool)>,
    /// Le curseur de volume est ouvert au-dessus de son panneau.
    slider_open: bool,
    /// Animation d'ouverture ou de fermeture du curseur : début, et ouverture de départ (0 à 1).
    slider_anim: Option<(Instant, f32)>,
    /// On fait glisser le curseur de volume.
    slider_drag: bool,
}

/// Les boutons du panneau du volume.
#[derive(Clone, Copy, PartialEq)]
enum Control {
    Volume,
    /// Haut-parleur du curseur de volume : coupe ou rétablit le son.
    Mute,
    /// Piste du curseur de volume.
    Slider,
}

/// Panneau collé à droite du Dock : le volume (le lecteur et l'heure sont dans l'encoche).
struct Panels {
    audio: RECT,
    /// Icône du volume, au milieu du panneau.
    volume: RECT,
    /// Curseur de volume au-dessus du panneau, quand il est affiché (ou en train de s'ouvrir ou
    /// de se fermer).
    slider: Option<SliderLayout>,
    /// Le curseur répond à la souris (pas pendant qu'il se referme).
    slider_live: bool,
}

/// Curseur vertical : le pourcentage en haut, la piste, le haut-parleur en bas.
struct SliderLayout {
    panel: RECT,
    mute: RECT,
    track: RECT,
    /// Zone cliquable de la piste, plus large que la piste elle-même.
    hit: RECT,
    percent: RECT,
}

impl SliderLayout {
    /// Volume correspondant à l'ordonnée `y` sur la piste (plein en haut).
    fn level_at(&self, y: i32) -> f32 {
        ((self.track.bottom - y) as f32 / (self.track.bottom - self.track.top).max(1) as f32).clamp(0.0, 1.0)
    }
}

impl Panels {
    fn control_at(&self, pt: POINT) -> Option<Control> {
        if let Some(s) = self.slider.as_ref().filter(|_| self.slider_live) {
            if contains(s.mute, pt) {
                return Some(Control::Mute);
            }
            if contains(s.hit, pt) {
                return Some(Control::Slider);
            }
        }
        contains(self.volume, pt).then_some(Control::Volume)
    }
}

impl DockUi {
    pub fn new(hwnd: HWND) -> Self {
        Self {
            hwnd,
            model: Dock::new(&dock::load_pins()),
            reserved: None,
            window: RECT::default(),
            dpi: 96,
            loader: shell::IconLoader::start(hwnd, WM_DOCK_LOADED),
            requested: HashSet::new(),
            backdrop: None,
            backdrop_rect: RECT::default(),
            backdrop_loaded: Arc::new(Mutex::new(None)),
            tracking: false,
            trash_full: false,
            hover_control: None,
            slider_open: false,
            slider_anim: None,
            slider_drag: false,
            clock: clock_text(),
            volume: None,
        }
    }

    fn request(&mut self, path: &str) {
        if self.requested.insert(path.to_owned()) {
            self.loader.request(path);
        }
    }

    /// Ouverture du curseur de volume : 0 fermé, 1 ouvert (un peu plus au rebond).
    fn slider_shown(&self, now: Instant) -> f32 {
        let target = if self.slider_open { 1.0 } else { 0.0 };
        let Some((start, from)) = self.slider_anim else { return target };
        let e = if self.slider_open {
            anim::spring(anim::progress(start, SLIDER_OPEN, now))
        } else {
            anim::ease_out(anim::progress(start, SLIDER_CLOSE, now))
        };
        from + (target - from) * e as f32
    }

    /// Le curseur est à l'écran, ouvert ou en train de s'ouvrir ou de se fermer.
    fn slider_visible(&self) -> bool {
        self.slider_open || self.slider_anim.is_some()
    }
}

impl App {
    pub(super) fn handle_dock_message(&mut self, msg: u32, wp: WPARAM, lp: LPARAM) -> bool {
        match msg {
            WM_DOCKBAR => match wp.0 as u32 {
                ABN_POSCHANGED => self.place_dock(),
                ABN_FULLSCREENAPP if self.enabled => unsafe {
                    let cmd = if lp.0 != 0 && win::fullscreen_app_in_front() { SW_HIDE } else { SW_SHOWNOACTIVATE };
                    let _ = ShowWindow(self.dock.hwnd, cmd);
                },
                _ => {}
            },
            WM_DOCK_LOADED => self.on_dock_loaded(),
            WM_TIMER if wp.0 == TIMER_CLOCK => {
                // Chaque seconde : le volume (il a pu changer au clavier) et l'heure de l'encoche.
                let volume = volume::get();
                if volume != self.dock.volume {
                    self.dock.volume = volume;
                    self.render_dock();
                }
                let clock = clock_text();
                if clock != self.dock.clock {
                    self.dock.clock = clock;
                    self.render_notch();
                }
            }
            WM_DOCK_WALLPAPER => {
                let loaded = self.dock.backdrop_loaded.lock().ok().and_then(|mut l| l.take());
                if let Some(image) = loaded {
                    self.dock.backdrop = Some(Rc::new(image));
                    self.render_dock();
                }
            }
            _ => return false,
        }
        true
    }

    // --- Mise en place ----------------------------------------------------------------

    pub(super) fn enable_dock(&mut self) {
        taskbar::autohide();
        let mut abd = APPBARDATA {
            cbSize: size_of::<APPBARDATA>() as u32,
            hWnd: self.dock.hwnd,
            uCallbackMessage: WM_DOCKBAR,
            ..Default::default()
        };
        unsafe { SHAppBarMessage(ABM_NEW, &mut abd) };
        self.dock.reserved = Some(RECT::default());
        let paths: Vec<String> = self.dock.model.items.iter().map(|i| i.path.clone()).collect();
        for path in paths {
            self.dock.request(&path);
        }
        self.update_running();
        self.place_dock();
        unsafe {
            let _ = ShowWindow(self.dock.hwnd, SW_SHOWNOACTIVATE);
            // L'horloge vérifie chaque seconde si la minute a changé.
            SetTimer(self.hwnd, TIMER_CLOCK, 1000, None);
        }
    }

    pub(super) fn disable_dock(&mut self) {
        unsafe {
            let _ = ShowWindow(self.dock.hwnd, SW_HIDE);
            let _ = KillTimer(self.hwnd, TIMER_CLOCK);
        }
        if self.dock.reserved.take().is_some() {
            let mut abd = APPBARDATA { cbSize: size_of::<APPBARDATA>() as u32, hWnd: self.dock.hwnd, ..Default::default() };
            unsafe { SHAppBarMessage(ABM_REMOVE, &mut abd) };
        }
        taskbar::restore();
    }

    /// L'explorateur a redémarré : il a oublié la place réservée au Dock.
    pub(super) fn reregister_dock(&mut self) {
        if self.dock.reserved.take().is_some() {
            self.disable_dock();
            self.enable_dock();
        }
    }

    /// Réserve la bande du bas de l'écran principal, puis place la fenêtre du Dock.
    fn place_dock(&mut self) {
        let Some(current) = self.dock.reserved else { return };
        let monitor = self.monitor;
        unsafe {
            let handle = MonitorFromPoint(POINT::default(), MONITOR_DEFAULTTOPRIMARY);
            let (mut dpi, mut dpi_y) = (96u32, 96u32);
            let _ = GetDpiForMonitor(handle, MDT_EFFECTIVE_DPI, &mut dpi, &mut dpi_y);
            self.dock.dpi = dpi as i32;
        }
        let m = Metrics::new(self.dock.dpi);
        let mut abd = APPBARDATA {
            cbSize: size_of::<APPBARDATA>() as u32,
            hWnd: self.dock.hwnd,
            uEdge: ABE_BOTTOM,
            rc: monitor,
            ..Default::default()
        };
        abd.rc.top = abd.rc.bottom - m.reserve;
        unsafe {
            SHAppBarMessage(ABM_QUERYPOS, &mut abd);
            abd.rc.top = abd.rc.bottom - m.reserve;
            if abd.rc != current {
                SHAppBarMessage(ABM_SETPOS, &mut abd);
                self.dock.reserved = Some(abd.rc);
            }
        }
        // Fond d'écran derrière toute la largeur du Dock (et de ses icônes grossies).
        let strip = RECT { left: monitor.left, top: monitor.bottom - 3 - m.height, right: monitor.right, bottom: monitor.bottom - 3 };
        if strip != self.dock.backdrop_rect {
            self.dock.backdrop_rect = strip;
            let blur = 10 * self.dock.dpi / 96;
            wallpaper::load_async(strip, monitor, blur, self.dock.hwnd, WM_DOCK_WALLPAPER, Arc::clone(&self.dock.backdrop_loaded));
        }
        self.render_dock();
    }

    /// Le fond d'écran a changé.
    pub(super) fn reload_dock_backdrop(&mut self) {
        self.dock.backdrop_rect = RECT::default();
        self.place_dock();
    }

    // --- Contenu ----------------------------------------------------------------------

    /// Repère les applis ouvertes et met le Dock à jour.
    pub(super) fn update_running(&mut self) {
        if self.dock.reserved.is_none() {
            return;
        }
        let mut running: Vec<String> = Vec::new();
        for h in win::top_level_windows() {
            if let Some(app) = self.app_key(h) {
                if !running.contains(&app) {
                    running.push(app);
                }
            }
        }
        let added = self.dock.model.set_running(&running, Instant::now());
        for path in added {
            self.dock.request(&path);
        }
        // La corbeille change d'icône quand elle se remplit ou se vide.
        let full = shell::recycle_bin_items() > 0;
        if full != self.dock.trash_full {
            self.dock.trash_full = full;
            self.dock.requested.remove(shell::RECYCLE_BIN);
            self.dock.request(shell::RECYCLE_BIN);
        }
        self.render_dock();
    }

    fn on_dock_loaded(&mut self) {
        let mut targets_changed = false;
        for loaded in self.dock.loader.take() {
            let icon = loaded.icon.map(Rc::new);
            for item in self.dock.model.items.iter_mut().filter(|i| i.path == loaded.path) {
                if !loaded.name.is_empty() {
                    item.name = loaded.name.clone();
                }
                if !loaded.target.is_empty() && item.target != loaded.target {
                    item.target = loaded.target.clone();
                    targets_changed = true;
                }
                if icon.is_some() {
                    item.icon = icon.clone();
                }
            }
        }
        // Noms et cibles connus : des applis ouvertes peuvent rejoindre leur icône épinglée.
        self.dock.model.merge_duplicates();
        if targets_changed {
            // On sait maintenant quel exécutable lance chaque raccourci : on revérifie qui tourne.
            self.update_running();
        } else {
            self.render_dock();
        }
    }

    // --- Dessin -----------------------------------------------------------------------

    fn dock_window_rect(&self, m: &Metrics) -> RECT {
        let mon = self.monitor;
        // Place pour le panneau du volume, des deux côtés pour que le Dock reste centré.
        let side = (VOLUME_WIDTH + PANEL_GAP) * self.dock.dpi / 96;
        let w = self.dock.model.window_width(m) + 2 * side;
        let left = (mon.left + mon.right - w) / 2;
        let bottom = mon.bottom - 3;
        // La fenêtre grandit vers le haut le temps que le curseur de volume est affiché (avec de
        // la marge pour son rebond) ; le reste du temps, elle reste basse.
        let extra = if self.dock.slider_visible() { (SLIDER_HEIGHT + PANEL_GAP + 30) * self.dock.dpi / 96 } else { 0 };
        RECT { left, top: bottom - m.height - extra, right: left + w, bottom }
    }

    /// Emplacement du panneau du volume, collé au bord droit du Dock (qui s'élargit au survol).
    fn panels(&self, body: RECT) -> Panels {
        let dpi = self.dock.dpi;
        let s = move |v: i32| v * dpi / 96;
        let gap = s(PANEL_GAP);
        let (cx, cy) = (body.right + gap + s(VOLUME_WIDTH) / 2, (body.top + body.bottom) / 2);
        let audio = RECT { left: body.right + gap, top: body.top, right: body.right + gap + s(VOLUME_WIDTH), bottom: body.bottom };
        let vsize = s(34);
        let volume = RECT { left: cx - vsize / 2, top: cy - vsize / 2, right: cx + vsize / 2, bottom: cy + vsize / 2 };
        // Curseur de volume : colonne au-dessus du bouton, de la largeur de son panneau.
        let slider = self.dock.slider_visible().then(|| {
            let panel = RECT { left: audio.left, top: audio.top - gap - s(SLIDER_HEIGHT), right: audio.right, bottom: audio.top - gap };
            let percent = RECT { left: panel.left, top: panel.top + s(10), right: panel.right, bottom: panel.top + s(30) };
            let mute = RECT { left: cx - s(17), top: panel.bottom - s(42), right: cx + s(17), bottom: panel.bottom - s(8) };
            let track = RECT { left: cx - s(3), top: percent.bottom + s(14), right: cx + s(3), bottom: mute.top - s(14) };
            let hit = RECT { left: panel.left, top: track.top - s(10), right: panel.right, bottom: track.bottom + s(10) };
            SliderLayout { panel, mute, track, hit, percent }
        });
        Panels { audio, volume, slider, slider_live: self.dock.slider_open }
    }

    /// Disposition actuelle du Dock et de ses panneaux, en coordonnées de sa fenêtre.
    fn dock_panels(&self) -> Panels {
        let m = Metrics::new(self.dock.dpi);
        let (w, h) = (self.dock.window.right - self.dock.window.left, self.dock.window.bottom - self.dock.window.top);
        self.panels(self.dock.model.layout(&m, w, h, Instant::now()).body)
    }

    pub(super) fn render_dock(&mut self) {
        if !self.enabled || self.dock.reserved.is_none() {
            return;
        }
        let m = Metrics::new(self.dock.dpi);
        let dpi = self.dock.dpi;
        let s = move |v: i32| v * dpi / 96;
        let window = self.dock_window_rect(&m);
        self.dock.window = window;
        let (w, h) = (window.right - window.left, window.bottom - window.top);
        let layout = self.dock.model.layout(&m, w, h, Instant::now());
        let body = layout.body;
        let white = (255, 255, 255);

        let mut canvas = Canvas::new(w, h);
        if self.dock.model.cursor.is_some() {
            // Zone invisible mais « pleine » au-dessus du Dock : la souris ne quitte pas la
            // fenêtre en passant entre deux icônes grossies.
            let lift = (m.max - m.icon) as i32 + s(12);
            canvas.round_rect(RECT { left: body.left - s(6), top: body.top - lift, right: body.right + s(6), bottom: h }, 0, (0, 0, 0), 1);
        }
        let radius = m.radius;
        // Style macOS d'avant Tahoe : gris foncé presque plein, fin liseré clair.
        let look = Material { tint: (38, 38, 42), opacity: 0.84, border: 0.14 };
        let origin = (window.left - self.dock.backdrop_rect.left, window.top - self.dock.backdrop_rect.top);
        // Panneau avec son ombre : le Dock, le volume et son curseur.
        let plate = |canvas: &mut Canvas, r: RECT, radius: f32, look: &Material| {
            canvas.round_rect(offset(inflate(r, s(1)), 0, s(3)), radius as i32 + s(1), (0, 0, 0), 45);
            canvas.material(r, radius, self.dock.backdrop.as_deref(), origin, look);
        };
        plate(&mut canvas, body, radius, &look);
        let panels = self.panels(body);
        plate(&mut canvas, panels.audio, radius, &look);
        if self.dock.hover_control == Some(Control::Volume) {
            let v = panels.volume;
            canvas.round_rect(v, (v.right - v.left) / 2, white, 40);
        }
        let (level, muted) = self.dock.volume.unwrap_or((0.0, false));
        draw_volume(&mut canvas, panels.volume, level, muted, dpi);
        // Curseur de volume, vertical au-dessus de son bouton. Il se déplie vers le haut en
        // apparaissant en fondu, et se replie de même.
        if let Some(sl) = &panels.slider {
            let shown = self.dock.slider_shown(Instant::now());
            if panels.slider_live {
                // Zone invisible mais « pleine » entre le bouton et le curseur : la souris peut passer
                // de l'un à l'autre sans quitter le Dock (ce qui fermerait le curseur).
                canvas.round_rect(RECT { top: sl.panel.top, ..panels.audio }, 0, (0, 0, 0), 1);
            }
            // Plaque : d'un carré posé sur le bouton jusqu'à toute sa hauteur.
            let full = sl.panel;
            let width = full.right - full.left;
            let height = width + ((full.bottom - full.top - width) as f32 * shown).round() as i32;
            let mut layer = Canvas::new(w, h);
            plate(&mut layer, RECT { top: full.bottom - height, ..full }, s(16) as f32, &look);
            // Contenu à sa place définitive, découvert à mesure que la plaque se déplie.
            let mut content = Canvas::new(w, h);
            if self.dock.hover_control == Some(Control::Mute) {
                content.round_rect(sl.mute, (sl.mute.right - sl.mute.left) / 2, white, 40);
            }
            draw_volume(&mut content, sl.mute, level, muted, dpi);
            let track = sl.track;
            let radius = (track.right - track.left) / 2;
            content.round_rect(track, radius, white, 70);
            let y = track.bottom - ((track.bottom - track.top) as f32 * level).round() as i32;
            let fill = if muted { 120 } else { 240 };
            if y < track.bottom {
                content.round_rect(RECT { top: y.min(track.bottom - 2 * radius), ..track }, radius, white, fill);
            }
            // Bouton rond, avec une ombre douce.
            let knob = s(16);
            let cx = (track.left + track.right) / 2;
            let knob_rect = RECT { left: cx - knob / 2, top: y - knob / 2, right: cx + knob / 2, bottom: y + knob / 2 };
            content.round_rect(offset(inflate(knob_rect, s(1)), 0, s(1)), knob / 2 + s(1), (0, 0, 0), 70);
            content.round_rect(knob_rect, knob / 2, white, 255);
            content.gdi(|dc| unsafe {
                SetBkMode(dc, TRANSPARENT);
                SelectObject(dc, self.font);
                let text = if muted { "—".to_owned() } else { format!("{} %", (level * 100.0).round()) };
                draw_text(dc, &text, sl.percent, DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX, TEXT, 0);
            });
            layer.overlay_inside(&content, 1.0);
            canvas.overlay(&layer, shown.clamp(0.0, 1.0));
        }
        if let Some(x) = layout.separator {
            canvas.round_rect(RECT { left: x, top: body.top + s(12), right: x + s(1).max(1), bottom: body.bottom - s(12) }, 0, white, 90);
        }
        let flat = |r: RECT| {
            let (l, t, rr, b) = (r.left as f32, r.top as f32, r.right as f32, r.bottom as f32);
            [(l, t), (rr, t), (rr, b), (l, b)]
        };
        for (item, rect) in self.dock.model.items.iter().zip(&layout.icons) {
            match &item.icon {
                Some(icon) => canvas.draw_quad(icon, flat(*rect), 255, 0),
                None => canvas.round_rect(inflate(*rect, -s(3)), s(12), (90, 90, 100), 200),
            }
            if item.running {
                // Point sous l'icône : l'appli est ouverte.
                let cx = (rect.left + rect.right) / 2;
                let dot = RECT { left: cx - s(2), top: body.bottom - s(6), right: cx + s(2), bottom: body.bottom - s(2) };
                canvas.round_rect(dot, s(2), white, 220);
            }
        }
        // Étiquette au-dessus de l'icône survolée, ou du volume.
        let volume_label = match self.dock.volume {
            Some((_, true)) => "Son coupé".to_owned(),
            Some((level, false)) => format!("Volume {} %", (level * 100.0).round()),
            None => "Aucune sortie audio".to_owned(),
        };
        let label = match self.dock.hover_control {
            Some(Control::Volume) if !self.dock.slider_open => Some((volume_label.as_str(), panels.volume)),
            _ => self.dock.model.hover.and_then(|i| Some((self.dock.model.items.get(i)?.name.as_str(), *layout.icons.get(i)?))),
        };
        let label_box = label.map(|(name, rect)| {
            let width = text_width(self.font, name) + s(22);
            let cx = (rect.left + rect.right) / 2;
            let bottom = rect.top - s(10);
            let r = RECT { left: cx - width / 2, top: bottom - s(26), right: cx + width / 2, bottom };
            offset(r, (s(4) - r.left).max(0) - (r.right - (w - s(4))).max(0), 0)
        });
        if let Some(r) = label_box {
            canvas.round_rect(r, s(8), (28, 28, 32), 225);
        }
        canvas.gdi(|dc| unsafe {
            SetBkMode(dc, TRANSPARENT);
            let centered = DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX;
            if let (Some((name, _)), Some(r)) = (label, label_box) {
                SelectObject(dc, self.font);
                SetTextColor(dc, TEXT);
                let mut text: Vec<u16> = name.encode_utf16().collect();
                let mut rect = r;
                DrawTextW(dc, &mut text, &mut rect, centered);
            }
        });
        canvas.present(self.dock.hwnd, POINT { x: window.left, y: window.top });
    }

    pub(super) fn dock_animating(&self) -> bool {
        self.dock.model.animating() || self.dock.slider_anim.is_some()
    }

    pub(super) fn dock_tick(&mut self, now: Instant) {
        let mut redraw = false;
        if let Some((start, _)) = self.dock.slider_anim {
            let duration = if self.dock.slider_open { SLIDER_OPEN } else { SLIDER_CLOSE };
            if anim::progress(start, duration, now) >= 1.0 {
                // Fin de l'animation ; une fois refermé, le curseur disparaît et la fenêtre
                // reprend sa hauteur.
                self.dock.slider_anim = None;
            }
            redraw = true;
        }
        if self.dock.model.animating() {
            self.dock.model.tick(now);
            redraw = true;
        }
        if redraw {
            self.render_dock();
        }
    }

    /// Ouvre ou referme le curseur de volume, en douceur.
    fn set_slider(&mut self, open: bool) {
        if self.dock.slider_open == open {
            return;
        }
        let now = Instant::now();
        let from = self.dock.slider_shown(now).clamp(0.0, 1.0);
        self.dock.slider_open = open;
        self.dock.slider_anim = Some((now, from));
        self.render_dock();
    }

    // --- Souris -----------------------------------------------------------------------

    /// Icône sous l'abscisse `x` (coordonnées de la fenêtre du Dock).
    fn dock_hit(&self, x: f32, y: i32) -> Option<usize> {
        let m = Metrics::new(self.dock.dpi);
        let (w, h) = (self.dock.window.right - self.dock.window.left, self.dock.window.bottom - self.dock.window.top);
        let layout = self.dock.model.layout(&m, w, h, Instant::now());
        let top = layout.icons.iter().map(|r| r.top).min().unwrap_or(layout.body.top);
        if y < top - (m.gap as i32) || x < layout.body.left as f32 || x > layout.body.right as f32 {
            return None;
        }
        let half_gap = m.gap / 2.0;
        layout.icons.iter().position(|r| x >= r.left as f32 - half_gap && x < r.right as f32 + half_gap)
    }

    pub(super) fn dock_mouse(&mut self, msg: u32, wp: WPARAM, lp: LPARAM) {
        let pt = point(lp);
        match msg {
            WM_MOUSEMOVE => {
                if !self.dock.tracking {
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: self.dock.hwnd,
                        dwHoverTime: 0,
                    };
                    self.dock.tracking = unsafe { TrackMouseEvent(&mut tme) }.is_ok();
                }
                if self.dock.slider_drag {
                    self.drag_volume(pt.y);
                    return;
                }
                let hover = self.dock_hit(pt.x as f32, pt.y);
                // Le Dock ne grossit que sous la souris, pas quand elle passe sur le volume.
                self.dock.model.set_hover(hover.map(|_| pt.x as f32), hover);
                self.dock.hover_control = self.dock_panels().control_at(pt);
                self.render_dock();
            }
            WM_MOUSELEAVE => {
                self.dock.tracking = false;
                self.dock.model.set_hover(None, None);
                self.dock.hover_control = None;
                self.render_dock();
                // La souris quitte le Dock : le curseur de volume se referme (sauf en plein glisser).
                if !self.dock.slider_drag {
                    self.set_slider(false);
                }
            }
            WM_LBUTTONDOWN => {
                if self.dock_panels().control_at(pt) == Some(Control::Slider) {
                    self.dock.slider_drag = true;
                    unsafe {
                        // Garde la souris même si elle sort de la piste en glissant.
                        SetCapture(self.dock.hwnd);
                    }
                    self.drag_volume(pt.y);
                }
            }
            WM_LBUTTONUP if self.dock.slider_drag => {
                self.dock.slider_drag = false;
                unsafe {
                    let _ = ReleaseCapture();
                }
            }
            WM_LBUTTONUP => match self.dock_panels().control_at(pt) {
                // Clic sur l'icône du volume : ouvre ou ferme le curseur.
                Some(Control::Volume) => {
                    self.dock.volume = volume::get();
                    self.set_slider(!self.dock.slider_open);
                }
                Some(Control::Mute) => {
                    volume::toggle_mute();
                    self.dock.volume = volume::get();
                    self.render_dock();
                }
                Some(Control::Slider) => {}
                // Ailleurs : le curseur se referme, et le clic fait son effet habituel.
                control => {
                    self.set_slider(false);
                    self.dock_control_click(control, pt);
                }
            },
            WM_MOUSEWHEEL => {
                // La molette au-dessus du volume (ou du curseur) le règle, par pas de 2 %.
                // (Pour cette molette, Windows donne la position à l'écran.)
                let win = self.dock.window;
                let local = POINT { x: pt.x - win.left, y: pt.y - win.top };
                let panels = self.dock_panels();
                if contains(panels.audio, local) || panels.slider.as_ref().is_some_and(|s| contains(s.panel, local)) {
                    let notches = ((wp.0 >> 16) & 0xFFFF) as i16 as f32 / 120.0;
                    let (level, _) = volume::get().unwrap_or((0.0, false));
                    volume::set(level + notches * 0.02);
                    self.dock.volume = volume::get();
                    if !self.dock.slider_open {
                        self.dock.hover_control = Some(Control::Volume);
                    }
                    self.render_dock();
                }
            }
            _ => {}
        }
    }

    /// Règle le volume d'après la position de la souris sur la piste du curseur.
    fn drag_volume(&mut self, y: i32) {
        let Some(slider) = self.dock_panels().slider else { return };
        volume::set(slider.level_at(y));
        self.dock.volume = volume::get();
        self.render_dock();
    }

    /// Clic sur une icône du Dock (les boutons du volume sont gérés par `dock_mouse`).
    fn dock_control_click(&mut self, control: Option<Control>, pt: POINT) {
        if control.is_none() {
            if let Some(index) = self.dock_hit(pt.x as f32, pt.y) {
                self.dock_click(index);
            }
        }
    }

    fn dock_click(&mut self, index: usize) {
        let Some(item) = self.dock.model.items.get(index) else { return };
        match item.kind {
            Kind::Trash => shell::open_recycle_bin(),
            Kind::App if item.running => self.show_app(index),
            Kind::App => {
                shell::open(&item.path);
                // L'icône rebondit jusqu'à l'ouverture d'une fenêtre de l'appli.
                if let Some(item) = self.dock.model.items.get_mut(index) {
                    item.launched = Some(Instant::now());
                }
            }
        }
    }

    /// Fenêtres ouvertes de l'appli d'un élément du Dock.
    fn app_windows(&mut self, index: usize) -> Vec<HWND> {
        let mut windows = Vec::new();
        for h in win::top_level_windows() {
            let Some(app) = self.app_key(h) else { continue };
            if self.dock.model.items.get(index).is_some_and(|i| i.matches(&app)) {
                windows.push(h);
            }
        }
        windows
    }

    /// Affiche une appli ouverte : sa scène si elle en a une, sinon sa fenêtre.
    fn show_app(&mut self, index: usize) {
        let windows = self.app_windows(index);
        if let Some(&h) = windows.iter().find(|h| self.model.is_tracked(wid_of(**h))) {
            let w = wid_of(h);
            match self.model.stage_of(w) {
                Some(id) if !self.model.in_active_stage(w) => {
                    let t = self.model.activate(id, Some(w));
                    self.apply(t, true);
                    self.refresh();
                }
                _ => win::force_foreground(h),
            }
        } else if let Some(&h) = windows.first() {
            if win::is_minimized(h) {
                win::show(h, SW_RESTORE);
            }
            win::force_foreground(h);
        }
    }

    fn quit_app(&mut self, index: usize) {
        for h in self.app_windows(index) {
            unsafe {
                let _ = PostMessageW(h, WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
    }

    fn toggle_pin(&mut self, index: usize) {
        self.dock.model.toggle_pin(index);
        dock::save_pins(&self.dock.model.pins());
        self.render_dock();
    }
}

/// Menu du clic droit sur une icône du Dock (ouvert hors de `with_app` : il a sa propre boucle).
pub(super) unsafe fn dock_menu(hwnd: HWND, pt: POINT) {
    let Some(Some((index, kind, running, pinned))) = with_app(|app| {
        let index = app.dock_hit(pt.x as f32, pt.y)?;
        let item = app.dock.model.items.get(index)?;
        Some((index, item.kind, item.running, item.pinned))
    }) else {
        return;
    };
    let mut items = Vec::new();
    let mut checked = Vec::new();
    match kind {
        Kind::Trash => {
            items.push((MENU_DOCK_OPEN, w!("Ouvrir")));
            items.push((MENU_TRASH_EMPTY, w!("Vider la corbeille")));
        }
        Kind::App => {
            items.push((MENU_DOCK_OPEN, if running { w!("Afficher") } else { w!("Ouvrir") }));
            items.push((MENU_DOCK_PIN, w!("Garder dans le Dock")));
            if pinned {
                checked.push(MENU_DOCK_PIN);
            }
            if running {
                items.push((0, PCWSTR::null()));
                items.push((MENU_DOCK_QUIT, w!("Quitter")));
            }
        }
    }
    match popup(hwnd, &items, &checked) {
        MENU_DOCK_OPEN => {
            with_app(|app| app.dock_click(index));
        }
        MENU_DOCK_PIN => {
            with_app(|app| app.toggle_pin(index));
        }
        MENU_DOCK_QUIT => {
            with_app(|app| app.quit_app(index));
        }
        MENU_TRASH_EMPTY => shell::empty_recycle_bin(hwnd),
        _ => {}
    }
}

/// Haut-parleur avec une à trois ondes selon le volume, ou une croix si le son est coupé.
fn draw_volume(canvas: &mut Canvas, button: RECT, level: f32, muted: bool, dpi: i32) {
    let k = dpi as f32 / 96.0;
    let (cx, cy) = ((button.left + button.right) as f32 / 2.0, (button.top + button.bottom) as f32 / 2.0);
    let white = (255, 255, 255);
    let x0 = cx - 9.0 * k;
    let body = RECT {
        left: x0.round() as i32,
        top: (cy - 3.0 * k).round() as i32,
        right: (x0 + 3.5 * k).round() as i32,
        bottom: (cy + 3.0 * k).round() as i32,
    };
    canvas.round_rect(body, k.round() as i32, white, 255);
    let cone = [(x0 + 3.0 * k, cy - 3.0 * k), (x0 + 8.0 * k, cy - 7.5 * k), (x0 + 8.0 * k, cy + 7.5 * k), (x0 + 3.0 * k, cy + 3.0 * k)];
    canvas.fill_convex(&round_corners(&cone, k), white, 255);
    if muted {
        // Croix : deux barres fines en diagonale.
        let (mx, r, t) = (x0 + 14.0 * k, 3.2 * k, 0.8 * k);
        for s in [1.0, -1.0] {
            let bar = [(mx - r - t, cy - s * (r - t)), (mx - r + t, cy - s * (r + t)), (mx + r + t, cy + s * (r - t)), (mx + r - t, cy + s * (r + t))];
            canvas.fill_convex(&bar, white, 255);
        }
        return;
    }
    // Ondes : allumées selon le volume, les autres restent discrètes.
    for (i, threshold) in [0.001, 0.34, 0.67].into_iter().enumerate() {
        let alpha = if level >= threshold { 255 } else { 60 };
        canvas.arc((x0 + 7.0 * k, cy), (4.5 + 3.2 * i as f32) * k, 0.8, 1.6 * k, white, alpha);
    }
}

/// Heure (« 22:15 ») et date courte (« jeu. 30 oct. ») au format de Windows.
pub(super) fn clock_text() -> (String, String) {
    use windows::Win32::Globalization::{GetDateFormatEx, GetTimeFormatEx};
    let read = |buf: &[u16], n: i32| String::from_utf16_lossy(&buf[..(n - 1).max(0) as usize]);
    unsafe {
        let mut time = [0u16; 32];
        let mut date = [0u16; 64];
        let n = GetTimeFormatEx(PCWSTR::null(), Default::default(), None, w!("HH:mm"), Some(&mut time));
        let d = GetDateFormatEx(PCWSTR::null(), Default::default(), None, w!("ddd d MMM"), Some(&mut date), PCWSTR::null());
        (read(&time, n), read(&date, d))
    }
}

/// Largeur d'un texte dans une police donnée, en pixels.
pub(super) fn text_width(font: HFONT, text: &str) -> i32 {
    unsafe {
        let dc = CreateCompatibleDC(None);
        let old = SelectObject(dc, font);
        let wide: Vec<u16> = text.encode_utf16().collect();
        let mut size = SIZE::default();
        let _ = GetTextExtentPoint32W(dc, &wide, &mut size);
        SelectObject(dc, old);
        let _ = DeleteDC(dc);
        size.cx
    }
}
