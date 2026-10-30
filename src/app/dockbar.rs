//! Le Dock à l'écran : sa fenêtre, sa place réservée en bas de l'écran, son dessin, ses clics.

use std::collections::HashSet;

use super::*;
use crate::{
    canvas::round_corners,
    dock::{self, Dock, Kind, Metrics},
    shell,
};

pub(super) const WM_DOCKBAR: u32 = WM_APP + 6;
pub(super) const WM_DOCK_LOADED: u32 = WM_APP + 7;
pub(super) const WM_DOCK_WALLPAPER: u32 = WM_APP + 8;

const TIMER_CLOCK: usize = 3;

/// Largeurs du lecteur (à gauche du Dock) et de l'horloge (à droite), et leur écart avec le
/// Dock, en pixels à 100 %.
const PLAYER_WIDTH: i32 = 250;
const CLOCK_WIDTH: i32 = 110;
const PANEL_GAP: i32 = 10;

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
    hover_play: bool,
    /// Heure et date affichées, pour ne redessiner qu'au changement de minute.
    clock: (String, String),
}

/// Panneaux collés au Dock : le lecteur à gauche (s'il y a de la musique), l'horloge à droite.
struct Panels {
    player: Option<PlayerLayout>,
    clock: RECT,
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
            hover_play: false,
            clock: clock_text(),
        }
    }

    fn request(&mut self, path: &str) {
        if self.requested.insert(path.to_owned()) {
            self.loader.request(path);
        }
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
                let clock = clock_text();
                if clock != self.dock.clock {
                    self.dock.clock = clock;
                    self.render_dock();
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
        // Place pour les panneaux des deux côtés, à égalité pour que le Dock reste centré.
        let side = (PLAYER_WIDTH.max(CLOCK_WIDTH) + PANEL_GAP) * self.dock.dpi / 96;
        let w = self.dock.model.window_width(m) + 2 * side;
        let left = (mon.left + mon.right - w) / 2;
        let bottom = mon.bottom - 3;
        RECT { left, top: bottom - m.height, right: left + w, bottom }
    }

    /// Emplacement du lecteur et de l'horloge, collés aux bords du Dock (qui s'élargit au survol).
    fn panels(&self, body: RECT) -> Panels {
        let dpi = self.dock.dpi;
        let s = move |v: i32| v * dpi / 96;
        let (gap, height) = (s(PANEL_GAP), body.bottom - body.top);
        let clock = RECT { left: body.right + gap, top: body.top, right: body.right + gap + s(CLOCK_WIDTH), bottom: body.bottom };
        let player = self.now_playing.as_ref().map(|_| {
            let panel = RECT { left: body.left - gap - s(PLAYER_WIDTH), top: body.top, right: body.left - gap, bottom: body.bottom };
            let pad = s(8);
            let art_size = height - 2 * pad;
            let art = RECT { left: panel.left + pad, top: panel.top + pad, right: panel.left + pad + art_size, bottom: panel.bottom - pad };
            let size = s(30);
            let cy = (panel.top + panel.bottom) / 2;
            let button = RECT { left: panel.right - pad - size, top: cy - size / 2, right: panel.right - pad, bottom: cy + size / 2 };
            let title = RECT { left: art.right + s(10), top: cy - s(19), right: button.left - s(8), bottom: cy };
            let artist = RECT { top: cy, bottom: cy + s(18), ..title };
            PlayerLayout { panel, art, title, artist, button }
        });
        Panels { player, clock }
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
        let look = Glass { frost: 0.18, darken: 0.04, refraction: s(10) as f32, bevel: s(14) as f32, rim: 0.8 };
        let origin = (window.left - self.dock.backdrop_rect.left, window.top - self.dock.backdrop_rect.top);
        // Plaque de verre avec son ombre : le Dock, le lecteur et l'horloge.
        let plate = |canvas: &mut Canvas, r: RECT, radius: f32, look: &Glass| {
            canvas.round_rect(offset(inflate(r, s(1)), 0, s(3)), radius as i32 + s(1), (0, 0, 0), 45);
            match &self.dock.backdrop {
                Some(backdrop) => canvas.glass(r, radius, backdrop, origin, look),
                None => canvas.round_rect(r, radius as i32, (30, 30, 36), 170),
            }
        };
        plate(&mut canvas, body, radius, &look);
        let panels = self.panels(body);
        plate(&mut canvas, panels.clock, radius, &look);
        let player = panels.player.as_ref().zip(self.now_playing.as_ref());
        if let Some((p, np)) = player {
            plate(&mut canvas, p.panel, radius, &look);
            canvas.round_rect(offset(p.art, 0, s(2)), s(8), (0, 0, 0), 70);
            match &np.art {
                Some(art) => canvas.draw_quad(art, [
                    (p.art.left as f32, p.art.top as f32),
                    (p.art.right as f32, p.art.top as f32),
                    (p.art.right as f32, p.art.bottom as f32),
                    (p.art.left as f32, p.art.bottom as f32),
                ], 255, 0),
                None => canvas.round_rect(p.art, s(6), (70, 70, 80), 255),
            }
            // Bouton lecture/pause : icône pleine posée sur le verre, rond clair au survol.
            if self.dock.hover_play {
                canvas.round_rect(p.button, (p.button.right - p.button.left) / 2, white, 40);
            }
            draw_play_pause(&mut canvas, p.button, np.playing, dpi);
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
        // Étiquette au-dessus de l'icône survolée.
        let label = self.dock.model.hover.and_then(|i| Some((self.dock.model.items.get(i)?, layout.icons.get(i)?)));
        let label_box = label.map(|(item, rect)| {
            let width = text_width(self.font, &item.name) + s(22);
            let cx = (rect.left + rect.right) / 2;
            let bottom = rect.top - s(10);
            let r = RECT { left: cx - width / 2, top: bottom - s(26), right: cx + width / 2, bottom };
            offset(r, (s(4) - r.left).max(0) - (r.right - (w - s(4))).max(0), 0)
        });
        if let Some(r) = label_box {
            canvas.round_rect(r, s(8), (28, 28, 32), 225);
        }
        let (time, date) = &self.dock.clock;
        canvas.gdi(|dc| unsafe {
            SetBkMode(dc, TRANSPARENT);
            let centered = DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX;
            if let (Some((item, _)), Some(r)) = (label, label_box) {
                SelectObject(dc, self.font);
                SetTextColor(dc, TEXT);
                let mut text: Vec<u16> = item.name.encode_utf16().collect();
                let mut rect = r;
                DrawTextW(dc, &mut text, &mut rect, centered);
            }
            // Horloge : l'heure en gras, la date en dessous.
            let c = panels.clock;
            let mid = (c.top + c.bottom) / 2;
            SelectObject(dc, self.font_time);
            draw_text(dc, time, RECT { top: c.top + s(6), bottom: mid + s(4), ..c }, centered, TEXT, 0);
            SelectObject(dc, self.font);
            draw_text(dc, date, RECT { top: mid + s(2), bottom: c.bottom - s(6), ..c }, centered, TEXT_DIM, 0);
            // Lecteur : titre, artiste et icône lecture/pause.
            if let Some((p, np)) = player {
                let line = DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX;
                SelectObject(dc, self.font_bold);
                draw_text(dc, &np.title, p.title, line, TEXT, 0);
                SelectObject(dc, self.font);
                draw_text(dc, &np.artist, p.artist, line, TEXT_DIM, 0);
            }
        });
        canvas.present(self.dock.hwnd, POINT { x: window.left, y: window.top });
    }

    pub(super) fn dock_animating(&self) -> bool {
        self.dock.model.animating()
    }

    pub(super) fn dock_tick(&mut self, now: Instant) {
        if self.dock.model.animating() {
            self.dock.model.tick(now);
            self.render_dock();
        }
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

    pub(super) fn dock_mouse(&mut self, msg: u32, lp: LPARAM) {
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
                let hover = self.dock_hit(pt.x as f32, pt.y);
                // Le Dock ne grossit que sous la souris, pas quand elle passe sur le lecteur ou
                // l'horloge.
                self.dock.model.set_hover(hover.map(|_| pt.x as f32), hover);
                self.dock.hover_play = self.dock_panels().player.is_some_and(|p| contains(p.button, pt));
                self.render_dock();
            }
            WM_MOUSELEAVE => {
                self.dock.tracking = false;
                self.dock.model.set_hover(None, None);
                self.render_dock();
            }
            WM_LBUTTONUP => {
                let panels = self.dock_panels();
                if panels.player.as_ref().is_some_and(|p| contains(p.button, pt)) {
                    self.media.toggle_play_pause();
                    // Affiché tout de suite, sans attendre la confirmation du lecteur.
                    if let Some(np) = self.now_playing.as_mut() {
                        np.playing = !np.playing;
                    }
                    self.render_dock();
                } else if contains(panels.clock, pt) {
                    // Comme l'horloge de la barre des tâches : calendrier et notifications.
                    shell::open("ms-actioncenter:");
                } else if let Some(index) = self.dock_hit(pt.x as f32, pt.y) {
                    self.dock_click(index);
                }
            }
            _ => {}
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

/// Icône pleine du bouton, comme dans les lecteurs d'Apple : deux barres arrondies pendant la
/// lecture (pour mettre en pause), un triangle aux coins arrondis sinon.
fn draw_play_pause(canvas: &mut Canvas, button: RECT, playing: bool, dpi: i32) {
    let k = dpi as f32 / 96.0;
    let (cx, cy) = ((button.left + button.right) as f32 / 2.0, (button.top + button.bottom) as f32 / 2.0);
    let white = (255, 255, 255);
    if playing {
        let (w, h, gap) = (5.0 * k, 16.0 * k, 5.0 * k);
        for x in [cx - gap / 2.0 - w, cx + gap / 2.0] {
            let bar = RECT { left: x.round() as i32, top: (cy - h / 2.0).round() as i32, right: (x + w).round() as i32, bottom: (cy + h / 2.0).round() as i32 };
            canvas.round_rect(bar, (1.6 * k).round() as i32, white, 255);
        }
    } else {
        // Décalé vers la droite : un triangle paraît centré quand son centre de gravité l'est.
        let (w, h) = (14.0 * k, 16.0 * k);
        let left = cx - w / 2.0 + 1.5 * k;
        let triangle = [(left, cy - h / 2.0), (left + w, cy), (left, cy + h / 2.0)];
        canvas.fill_convex(&round_corners(&triangle, 2.2 * k), white, 255);
    }
}

/// Heure (« 22:15 ») et date courte (« jeu. 30 oct. ») au format de Windows.
fn clock_text() -> (String, String) {
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
fn text_width(font: HFONT, text: &str) -> i32 {
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
