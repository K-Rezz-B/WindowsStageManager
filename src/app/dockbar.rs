//! La barre d'applis : une pastille noire en haut de chaque écran, à gauche de l'encoche (les
//! scènes sont à droite). Elle remplace le Dock : le lanceur, les applis épinglées et ouvertes,
//! la corbeille et le volume, sans prendre de place à l'écran. La barre des tâches de Windows
//! reste masquée tant qu'elle est en service.

use std::{
    collections::HashSet,
    f32::consts::{FRAC_1_SQRT_2, PI},
};

use super::*;
use crate::{
    canvas::round_corners,
    dock::{self, Dock, Kind},
    shell, volume,
};

pub(super) const WM_DOCK_LOADED: u32 = WM_APP + 7;

pub(super) const TIMER_CLOCK: usize = 3;

/// Applis au plus dans la pastille (les suivantes n'y tiennent pas).
const MAX_APPS: usize = 20;
/// Écart entre les applis et la corbeille, séparateur compris (pixels à 100 %).
const SEP: i32 = 21;
/// Place à gauche de la pastille pour l'étiquette de la première icône, et hauteur de
/// l'étiquette, sous la pastille.
const SIDE: i32 = 80;
const LABEL_GAP: i32 = 8;
const LABEL_H: i32 = 26;
/// Curseur de volume, vertical sous son bouton : largeur, hauteur, écart avec la pastille.
const SLIDER_W: i32 = 44;
const SLIDER_HEIGHT: i32 = 190;
const PANEL_GAP: i32 = 8;
/// Ouverture (avec un léger rebond) et fermeture du curseur.
const SLIDER_OPEN: Duration = Duration::from_millis(320);
const SLIDER_CLOSE: Duration = Duration::from_millis(180);
/// Une appli qu'on vient de lancer sautille sous la pastille jusqu'à l'ouverture de sa fenêtre.
const BOUNCE: i32 = 3;
const BOUNCE_PERIOD: f32 = 0.55;

const MENU_DOCK_OPEN: usize = 20;
const MENU_DOCK_PIN: usize = 21;
const MENU_DOCK_QUIT: usize = 22;
const MENU_TRASH_EMPTY: usize = 23;

pub(super) struct DockUi {
    pub model: Dock,
    /// La barre d'applis est en service.
    pub enabled: bool,
    loader: shell::IconLoader,
    requested: HashSet<String>,
    trash_full: bool,
    /// Dernière fois qu'on a regardé la corbeille : Windows la parcourt pour la compter.
    trash_checked: Option<Instant>,
    /// Écran dont la barre est sous la souris : lui seul montre le survol et le curseur de volume.
    active: Option<ScreenId>,
    tracking: bool,
    /// Élément sous la souris, celui d'avant (il reprend sa taille) et le début du grossissement.
    hover: Option<Part>,
    hover_prev: Option<Part>,
    hover_start: Option<Instant>,
    /// Heure et date, pour redessiner l'heure de l'encoche au changement de minute.
    clock: (String, String),
    /// Volume (entre 0 et 1) et son coupé, relus chaque seconde.
    volume: Option<(f32, bool)>,
    /// Le curseur de volume est ouvert sous son bouton.
    slider_open: bool,
    /// Animation d'ouverture ou de fermeture du curseur : début, et ouverture de départ (0 à 1).
    slider_anim: Option<(Instant, f32)>,
    /// On fait glisser le curseur de volume.
    slider_drag: bool,
}

/// Les éléments de la barre.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Part {
    Launcher,
    /// Appli ou corbeille (rang dans le modèle).
    Item(usize),
    Volume,
    /// Haut-parleur du curseur de volume : coupe ou rétablit le son.
    Mute,
    /// Piste du curseur de volume.
    Slider,
}

/// Emplacement des éléments, en coordonnées de la fenêtre de la barre.
struct Layout {
    size: SIZE,
    pill: RECT,
    launcher: Option<RECT>,
    /// (rang dans le modèle, case) des applis affichées et de la corbeille.
    items: Vec<(usize, RECT)>,
    separator: Option<i32>,
    volume: RECT,
    slider: Option<SliderLayout>,
}

/// Curseur vertical : le haut-parleur en haut, la piste, le pourcentage en bas.
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

impl Layout {
    /// Élément sous `pt`. Les cases couvrent toute la hauteur de la pastille et la moitié des
    /// écarts : la souris ne « tombe » pas entre deux icônes.
    fn part_at(&self, pt: POINT, slider_live: bool, half_gap: i32) -> Option<Part> {
        if let Some(s) = self.slider.as_ref().filter(|_| slider_live) {
            if contains(s.mute, pt) {
                return Some(Part::Mute);
            }
            if contains(s.hit, pt) {
                return Some(Part::Slider);
            }
        }
        let cell = |r: RECT| RECT { left: r.left - half_gap, top: self.pill.top, right: r.right + half_gap, bottom: self.pill.bottom };
        if self.launcher.is_some_and(|r| contains(cell(r), pt)) {
            return Some(Part::Launcher);
        }
        if let Some(&(i, _)) = self.items.iter().find(|(_, r)| contains(cell(*r), pt)) {
            return Some(Part::Item(i));
        }
        contains(cell(self.volume), pt).then_some(Part::Volume)
    }

    fn rect_of(&self, part: Part) -> Option<RECT> {
        match part {
            Part::Launcher => self.launcher,
            Part::Item(i) => self.items.iter().find(|(j, _)| *j == i).map(|(_, r)| *r),
            Part::Volume => Some(self.volume),
            Part::Mute | Part::Slider => None,
        }
    }
}

impl DockUi {
    /// `hwnd` : la fenêtre principale, qui reçoit les icônes chargées.
    pub fn new(hwnd: HWND) -> Self {
        Self {
            model: Dock::new(&dock::load_pins()),
            enabled: false,
            loader: shell::IconLoader::start(hwnd, WM_DOCK_LOADED),
            requested: HashSet::new(),
            trash_full: false,
            trash_checked: None,
            active: None,
            tracking: false,
            hover: None,
            hover_prev: None,
            hover_start: None,
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

    /// Grossissement d'un élément au survol (0 au repos, 1 grossi), avec un petit rebond.
    fn grown(&self, part: Part, now: Instant) -> f64 {
        let e = self.hover_start.map_or(1.0, |start| anim::spring(anim::progress(start, HOVER_ANIM, now)));
        if self.hover == Some(part) {
            e
        } else if self.hover_prev == Some(part) {
            1.0 - e
        } else {
            0.0
        }
    }
}

impl App {
    pub(super) fn handle_dock_message(&mut self, msg: u32, wp: WPARAM, _lp: LPARAM) -> bool {
        match msg {
            WM_DOCK_LOADED => self.on_dock_loaded(),
            WM_TIMER if wp.0 == TIMER_CLOCK => {
                // Chaque seconde : le volume (il a pu changer au clavier) et l'heure de l'encoche.
                let volume = volume::get();
                if volume != self.dock.volume {
                    self.dock.volume = volume;
                    self.render_all_apps();
                }
                let clock = clock_text();
                if clock != self.dock.clock {
                    self.dock.clock = clock;
                    self.render_notches();
                }
            }
            _ => return false,
        }
        true
    }

    // --- Mise en place ----------------------------------------------------------------

    pub(super) fn enable_dock(&mut self) {
        taskbar::autohide();
        self.dock.enabled = true;
        let paths: Vec<String> = self.dock.model.items.iter().map(|i| i.path.clone()).collect();
        for path in paths {
            self.dock.request(&path);
        }
        self.update_running();
        // Les scènes se calent face à la barre d'applis, même sans encoche.
        for s in 0..self.screens.len() {
            self.place_appbar(s);
            self.place_apps(s);
        }
    }

    pub(super) fn disable_dock(&mut self) {
        self.dock.enabled = false;
        self.dock.slider_open = false;
        self.dock.slider_anim = None;
        self.dock.hover = None;
        self.dock.active = None;
        for s in 0..self.screens.len() {
            self.place_apps(s);
            self.place_appbar(s);
        }
        taskbar::restore();
    }

    /// Montre (ou cache) la barre d'applis d'un écran, et la redessine à sa place.
    pub(super) fn place_apps(&mut self, s: usize) {
        let sc = &self.screens[s];
        let shown = self.dock.enabled && !sc.fullscreen;
        unsafe {
            let _ = ShowWindow(sc.apps, if shown { SW_SHOWNOACTIVATE } else { SW_HIDE });
        }
        if shown {
            self.render_apps(s);
        }
    }

    // --- Contenu ----------------------------------------------------------------------

    /// Repère les applis ouvertes et met la barre à jour.
    pub(super) fn update_running(&mut self) {
        if !self.dock.enabled {
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
        // Des éléments ont pu partir : le survol ne doit pas pointer à côté.
        let count = self.dock.model.items.len();
        for part in [&mut self.dock.hover, &mut self.dock.hover_prev] {
            if matches!(part, Some(Part::Item(i)) if *i >= count) {
                *part = None;
            }
        }
        // La corbeille change d'icône quand elle se remplit ou se vide (vérifié au plus une fois
        // par seconde : chaque changement de premier plan passe par ici).
        let now = Instant::now();
        let due = self.dock.trash_checked.is_none_or(|at| now.duration_since(at) >= Duration::from_secs(1));
        if due {
            self.dock.trash_checked = Some(now);
        }
        let full = if due { shell::recycle_bin_items() > 0 } else { self.dock.trash_full };
        if full != self.dock.trash_full {
            self.dock.trash_full = full;
            self.dock.requested.remove(shell::RECYCLE_BIN);
            self.dock.request(shell::RECYCLE_BIN);
        }
        self.render_all_apps();
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
            self.render_all_apps();
        }
    }

    // --- Disposition --------------------------------------------------------------------

    /// La pastille et ses éléments, de gauche à droite : le lanceur, les applis, la corbeille
    /// et le volume, contre l'encoche.
    fn apps_layout(&self, s: usize) -> Layout {
        let px = |v: i32| self.px(s, v);
        let (icon, gap) = (px(ICON), px(ICON_GAP));
        let launcher = self.features.launcher;
        let trash = self.dock.model.items.iter().position(|i| i.kind == Kind::Trash);
        let apps: Vec<usize> =
            self.dock.model.items.iter().enumerate().filter(|(_, i)| i.kind == Kind::App).map(|(i, _)| i).take(MAX_APPS).collect();
        let before = launcher as usize + apps.len();
        let slots = before as i32 + trash.is_some() as i32 + 1;
        let separator = before > 0;
        let pill_w = 2 * px(PILL_PAD) + slots * icon + (slots - 1) * gap + if separator { px(SEP) - gap } else { 0 };

        let dropdown = if self.dock.slider_visible() { px(PANEL_GAP + SLIDER_HEIGHT) } else { px(LABEL_GAP + LABEL_H) };
        let size = SIZE { cx: px(SIDE) + pill_w + px(PILL_SHOULDER) + px(2), cy: px(BAR_H) + dropdown + px(12) };
        let right = size.cx - px(PILL_SHOULDER) - px(2);
        let pill = RECT { left: right - pill_w, top: 0, right, bottom: px(BAR_H) };
        let top = (pill.bottom - icon) / 2;
        let mut x = pill.left + px(PILL_PAD);
        let slot = |x: &mut i32| {
            let r = RECT { left: *x, top, right: *x + icon, bottom: top + icon };
            *x += icon + gap;
            r
        };
        let launcher = launcher.then(|| slot(&mut x));
        let mut items: Vec<(usize, RECT)> = apps.iter().map(|&i| (i, slot(&mut x))).collect();
        let separator = separator.then(|| {
            let at = x - gap + (px(SEP) - 1) / 2;
            x += px(SEP) - gap;
            at
        });
        if let Some(t) = trash {
            items.push((t, slot(&mut x)));
        }
        let volume = slot(&mut x);

        let slider = self.dock.slider_visible().then(|| {
            let cx = (volume.left + volume.right) / 2;
            let half = px(SLIDER_W) / 2;
            let panel = RECT { left: cx - half, top: pill.bottom + px(PANEL_GAP), right: cx + half, bottom: pill.bottom + px(PANEL_GAP + SLIDER_HEIGHT) };
            let mute = RECT { left: cx - px(15), top: panel.top + px(7), right: cx + px(15), bottom: panel.top + px(37) };
            let percent = RECT { left: panel.left, top: panel.bottom - px(30), right: panel.right, bottom: panel.bottom - px(10) };
            let track = RECT { left: cx - px(3), top: mute.bottom + px(14), right: cx + px(3), bottom: percent.top - px(12) };
            let hit = RECT { left: panel.left, top: track.top - px(10), right: panel.right, bottom: track.bottom + px(10) };
            SliderLayout { panel, mute, track, hit, percent }
        });
        Layout { size, pill, launcher, items, separator, volume, slider }
    }

    /// Position de la fenêtre de la barre d'applis : son bord droit à `NOTCH_GAP` de l'encoche
    /// (ou du milieu de l'écran, sans encoche).
    fn apps_window(&self, s: usize, size: SIZE) -> RECT {
        let sc = &self.screens[s];
        let half = self.notch_edge(s).unwrap_or(0);
        let cx = (sc.rect.left + sc.rect.right) / 2;
        let right = cx - half - self.px(s, NOTCH_GAP) + self.px(s, PILL_SHOULDER) + self.px(s, 2);
        RECT { left: right - size.cx, top: sc.rect.top, right, bottom: sc.rect.top + size.cy }
    }

    // --- Dessin -----------------------------------------------------------------------

    pub(super) fn render_all_apps(&mut self) {
        for s in 0..self.screens.len() {
            self.render_apps(s);
        }
    }

    pub(super) fn render_apps(&mut self, s: usize) {
        if !self.dock.enabled || self.screens[s].fullscreen {
            return;
        }
        let now = anim::now();
        let layout = self.apps_layout(s);
        let window = self.apps_window(s, layout.size);
        self.screens[s].apps_rect = window;
        let sc = &self.screens[s];
        let dpi = sc.dpi;
        let px = move |v: i32| v * dpi / 96;
        let white = (255, 255, 255);
        let active = self.dock.active == Some(sc.id);
        let (w, h) = (layout.size.cx, layout.size.cy);
        let mut canvas = Canvas::new(w, h);

        // Rentrée dans le bord de l'écran pendant que l'encoche est ouverte, comme les scènes.
        let shown = self.bar_shown(s, now);
        if shown <= 0.0 {
            canvas.present(sc.apps, POINT { x: window.left, y: window.top });
            return;
        }
        let lift = ((1.0 - shown) * px(BAR_H + 4) as f64).round() as i32;
        let up = |r: RECT| offset(r, 0, -lift);
        let mut pill = Canvas::new(w, h);
        pill.notch(up(layout.pill), px(PILL_RADIUS) as f32, px(PILL_SHOULDER) as f32, (0, 0, 0), 255);

        // Une icône, grossie au survol (vers le bas : le haut de la pastille colle au bord).
        let grow = |part: Part, r: RECT| -> [(f32, f32); 4] {
            let scale = if active { 1.0 + HOVER_SCALE * self.dock.grown(part, now) } else { 1.0 };
            let half = (r.right - r.left) as f64 * scale / 2.0;
            let (cx, cy) = ((r.left + r.right) as f64 / 2.0, (r.top + r.bottom) as f64 / 2.0);
            let (l, t, rr, b) = ((cx - half) as f32, (cy - half) as f32, (cx + half) as f32, (cy + half) as f32);
            [(l, t), (rr, t), (rr, b), (l, b)]
        };
        let bounds = |q: [(f32, f32); 4]| RECT { left: q[0].0 as i32, top: q[0].1 as i32, right: q[2].0 as i32, bottom: q[2].1 as i32 };

        if let Some(r) = layout.launcher {
            draw_magnifier(&mut pill, bounds(grow(Part::Launcher, up(r))));
        }
        for &(i, r) in &layout.items {
            let item = &self.dock.model.items[i];
            let bob = item.launched.map_or(0, |t| {
                let t = now.duration_since(t).as_secs_f32();
                ((PI * t / BOUNCE_PERIOD).sin().abs() * px(BOUNCE) as f32).round() as i32
            });
            let quad = grow(Part::Item(i), offset(up(r), 0, -bob));
            match &item.icon {
                Some(icon) => pill.draw_quad(icon, quad, 255, 0),
                None => pill.round_rect(inflate(bounds(quad), -px(2)), px(5), (90, 90, 100), 220),
            }
            if item.running {
                // Point sous l'icône : l'appli est ouverte.
                let cx = (r.left + r.right) / 2;
                let dot = up(RECT { left: cx - px(2), top: layout.pill.bottom - px(5), right: cx + px(2), bottom: layout.pill.bottom - px(1) });
                pill.round_rect(dot, px(2), white, 220);
            }
        }
        if let Some(x) = layout.separator {
            let sep = up(RECT { left: x, top: layout.pill.top + px(9), right: x + px(1).max(1), bottom: layout.pill.bottom - px(9) });
            pill.round_rect(sep, 0, white, 70);
        }
        let (level, muted) = self.dock.volume.unwrap_or((0.0, false));
        draw_volume(&mut pill, bounds(grow(Part::Volume, up(layout.volume))), level, muted);
        canvas.overlay(&pill, shown as f32);

        if active && shown >= 1.0 {
            self.draw_apps_extras(&mut canvas, &layout, s, now);
        }
        canvas.present(sc.apps, POINT { x: window.left, y: window.top });
    }

    /// Sous la pastille : le curseur de volume, ou l'étiquette de l'élément survolé.
    fn draw_apps_extras(&self, canvas: &mut Canvas, layout: &Layout, s: usize, now: Instant) {
        let sc = &self.screens[s];
        let dpi = sc.dpi;
        let px = move |v: i32| v * dpi / 96;
        let (w, h) = (layout.size.cx, layout.size.cy);
        let white = (255, 255, 255);
        let font = sc.font;
        let (level, muted) = self.dock.volume.unwrap_or((0.0, false));
        let look = Material { tint: (30, 30, 32), opacity: 0.96, border: 0.14 };

        // Curseur de volume, vertical sous son bouton. Il se déplie vers le bas en apparaissant en
        // fondu, et se replie de même.
        if let Some(sl) = &layout.slider {
            let shown = self.dock.slider_shown(now);
            if self.dock.slider_open {
                // Zone invisible mais « pleine » entre le bouton et le curseur : la souris peut passer
                // de l'un à l'autre sans quitter la barre (ce qui fermerait le curseur).
                canvas.round_rect(RECT { top: layout.pill.bottom - px(2), bottom: sl.panel.top + px(2), ..sl.panel }, 0, (0, 0, 0), 1);
            }
            // Plaque : d'un carré sous le bouton jusqu'à toute sa hauteur.
            let full = sl.panel;
            let width = full.right - full.left;
            let height = width + ((full.bottom - full.top - width) as f32 * shown).round() as i32;
            let plate = RECT { bottom: full.top + height, ..full };
            let mut layer = Canvas::new(w, h);
            layer.round_rect(offset(inflate(plate, px(1)), 0, px(3)), px(15), (0, 0, 0), 45);
            layer.material(plate, px(14) as f32, None, (0, 0), &look);
            // Contenu à sa place définitive, découvert à mesure que la plaque se déplie.
            let mut content = Canvas::new(w, h);
            if self.dock.hover == Some(Part::Mute) {
                content.round_rect(sl.mute, (sl.mute.right - sl.mute.left) / 2, white, 40);
            }
            draw_volume(&mut content, inflate(sl.mute, -px(5)), level, muted);
            let track = sl.track;
            let radius = (track.right - track.left) / 2;
            content.round_rect(track, radius, white, 70);
            let y = track.bottom - ((track.bottom - track.top) as f32 * level).round() as i32;
            let fill = if muted { 120 } else { 240 };
            if y < track.bottom {
                content.round_rect(RECT { top: y.min(track.bottom - 2 * radius), ..track }, radius, white, fill);
            }
            // Bouton rond, avec une ombre douce.
            let knob = px(16);
            let cx = (track.left + track.right) / 2;
            let knob_rect = RECT { left: cx - knob / 2, top: y - knob / 2, right: cx + knob / 2, bottom: y + knob / 2 };
            content.round_rect(offset(inflate(knob_rect, px(1)), 0, px(1)), knob / 2 + px(1), (0, 0, 0), 70);
            content.round_rect(knob_rect, knob / 2, white, 255);
            content.gdi(|dc| unsafe {
                SetBkMode(dc, TRANSPARENT);
                SelectObject(dc, font);
                let text = if muted { "—".to_owned() } else { format!("{} %", (level * 100.0).round()) };
                draw_text(dc, &text, sl.percent, DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX, TEXT, 0);
            });
            layer.overlay_inside(&content, 1.0);
            canvas.overlay(&layer, shown.clamp(0.0, 1.0));
            return;
        }

        // Étiquette sous l'élément survolé.
        let volume_label = match self.dock.volume {
            Some((_, true)) => "Son coupé".to_owned(),
            Some((level, false)) => format!("Volume {} %", (level * 100.0).round()),
            None => "Aucune sortie audio".to_owned(),
        };
        let Some(part) = self.dock.hover else { return };
        let name = match part {
            Part::Launcher => "Rechercher",
            Part::Volume => volume_label.as_str(),
            Part::Item(i) => match self.dock.model.items.get(i) {
                Some(item) => item.name.as_str(),
                None => return,
            },
            Part::Mute | Part::Slider => return,
        };
        let Some(rect) = layout.rect_of(part) else { return };
        let width = text_width(font, name) + px(22);
        let cx = (rect.left + rect.right) / 2;
        let top = layout.pill.bottom + px(LABEL_GAP);
        let r = RECT { left: cx - width / 2, top, right: cx + width / 2, bottom: top + px(LABEL_H) };
        let r = offset(r, (px(4) - r.left).max(0) - (r.right - (w - px(4))).max(0), 0);
        canvas.round_rect(r, px(8), (28, 28, 32), 230);
        canvas.gdi(|dc| unsafe {
            SetBkMode(dc, TRANSPARENT);
            SelectObject(dc, font);
            draw_text(dc, name, r, DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX, TEXT, 0);
        });
    }

    pub(super) fn dock_animating(&self) -> bool {
        self.dock.enabled && (self.dock.model.animating() || self.dock.slider_anim.is_some() || self.dock.hover_start.is_some())
    }

    /// Avance les animations du Dock. Renvoie `true` s'il faut redessiner toutes les barres
    /// d'applis.
    pub(super) fn dock_tick(&mut self, now: Instant) -> bool {
        if !self.dock.enabled {
            return false;
        }
        let mut redraw = self.dock.model.animating();
        if let Some((start, _)) = self.dock.slider_anim {
            let duration = if self.dock.slider_open { SLIDER_OPEN } else { SLIDER_CLOSE };
            if anim::progress(start, duration, now) >= 1.0 {
                // Fin de l'animation ; une fois refermé, le curseur disparaît.
                self.dock.slider_anim = None;
            }
            redraw = true;
        }
        if let Some(start) = self.dock.hover_start {
            if anim::progress(start, HOVER_ANIM, now) >= 1.0 {
                self.dock.hover_start = None;
            }
            redraw = true;
        }
        redraw
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
        self.render_all_apps();
    }

    // --- Souris -----------------------------------------------------------------------

    fn apps_part(&self, s: usize, pt: POINT) -> Option<Part> {
        let layout = self.apps_layout(s);
        layout.part_at(pt, self.dock.slider_open, self.px(s, ICON_GAP) / 2)
    }

    fn set_apps_hover(&mut self, hover: Option<Part>) {
        if self.dock.hover != hover {
            self.dock.hover_prev = self.dock.hover;
            self.dock.hover = hover;
            self.dock.hover_start = Some(Instant::now());
        }
    }

    /// Souris sur la barre d'applis de la fenêtre `hwnd`.
    pub(super) fn dock_mouse(&mut self, hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) {
        let Some(s) = self.apps_index(hwnd) else { return };
        let pt = point(lp);
        let id = self.screens[s].id;
        match msg {
            WM_MOUSEMOVE => {
                // Redessinée seulement si quelque chose change sous la souris.
                let mut changed = false;
                if self.dock.active != Some(id) {
                    changed = true;
                    // La souris passe d'un écran à l'autre : le curseur de volume reste où il était.
                    let previous = self.dock.active.replace(id).and_then(|p| self.screen_index(p));
                    self.dock.tracking = false;
                    self.dock.slider_open = false;
                    self.dock.slider_anim = None;
                    if let Some(p) = previous {
                        self.render_apps(p);
                    }
                }
                if !self.dock.tracking {
                    let mut tme = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                    self.dock.tracking = unsafe { TrackMouseEvent(&mut tme) }.is_ok();
                }
                if self.dock.slider_drag {
                    self.drag_volume(s, pt.y);
                    return;
                }
                let part = self.apps_part(s, pt);
                changed |= self.dock.hover != part;
                self.set_apps_hover(part);
                if changed {
                    self.render_apps(s);
                }
            }
            WM_MOUSELEAVE => {
                self.dock.tracking = false;
                self.set_apps_hover(None);
                // La souris quitte la barre : le curseur de volume se referme (sauf en plein glisser).
                if !self.dock.slider_drag {
                    self.set_slider(false);
                }
                self.render_apps(s);
            }
            WM_LBUTTONDOWN => {
                if self.apps_part(s, pt) == Some(Part::Slider) {
                    self.dock.slider_drag = true;
                    unsafe {
                        // Garde la souris même si elle sort de la piste en glissant.
                        SetCapture(hwnd);
                    }
                    self.drag_volume(s, pt.y);
                }
            }
            WM_LBUTTONUP if self.dock.slider_drag => {
                self.dock.slider_drag = false;
                unsafe {
                    let _ = ReleaseCapture();
                }
            }
            WM_LBUTTONUP => match self.apps_part(s, pt) {
                // Clic sur l'icône du volume : ouvre ou ferme le curseur.
                Some(Part::Volume) => {
                    self.dock.volume = volume::get();
                    self.set_slider(!self.dock.slider_open);
                }
                Some(Part::Mute) => {
                    volume::toggle_mute();
                    self.dock.volume = volume::get();
                    self.render_apps(s);
                }
                Some(Part::Slider) => {}
                // Ailleurs : le curseur se referme, et le clic fait son effet habituel.
                Some(Part::Launcher) => {
                    self.set_slider(false);
                    self.open_launcher();
                }
                Some(Part::Item(i)) => {
                    self.set_slider(false);
                    self.dock_click(i);
                }
                None => self.set_slider(false),
            },
            WM_MOUSEWHEEL => {
                // La molette au-dessus du volume (ou du curseur) le règle, par pas de 2 %.
                // (Pour cette molette, Windows donne la position à l'écran.)
                let win = self.screens[s].apps_rect;
                let local = POINT { x: pt.x - win.left, y: pt.y - win.top };
                let layout = self.apps_layout(s);
                let over = matches!(layout.part_at(local, true, self.px(s, ICON_GAP) / 2), Some(Part::Volume | Part::Mute | Part::Slider))
                    || layout.slider.as_ref().is_some_and(|sl| contains(sl.panel, local));
                if over {
                    let notches = ((wp.0 >> 16) & 0xFFFF) as i16 as f32 / 120.0;
                    let (level, _) = volume::get().unwrap_or((0.0, false));
                    volume::set(level + notches * 0.02);
                    self.dock.volume = volume::get();
                    if !self.dock.slider_open {
                        self.set_apps_hover(Some(Part::Volume));
                    }
                    self.render_apps(s);
                }
            }
            _ => {}
        }
    }

    /// Règle le volume d'après la position de la souris sur la piste du curseur.
    fn drag_volume(&mut self, s: usize, y: i32) {
        let Some(slider) = self.apps_layout(s).slider else { return };
        volume::set(slider.level_at(y));
        self.dock.volume = volume::get();
        self.render_apps(s);
    }

    fn dock_click(&mut self, index: usize) {
        let Some(item) = self.dock.model.items.get(index) else { return };
        match item.kind {
            Kind::Trash => shell::open_recycle_bin(),
            Kind::App if item.running => self.show_app(index),
            Kind::App => {
                shell::open(&item.path);
                // L'icône sautille jusqu'à l'ouverture d'une fenêtre de l'appli.
                if let Some(item) = self.dock.model.items.get_mut(index) {
                    item.launched = Some(Instant::now());
                }
            }
        }
    }

    /// Fenêtres ouvertes de l'appli d'un élément de la barre.
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
                    if let Some(screen) = self.model.screen_of(w) {
                        self.apply(screen, t, true);
                    }
                    self.refresh_all();
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
        self.dock.hover = None;
        self.dock.hover_prev = None;
        dock::save_pins(&self.dock.model.pins());
        self.render_all_apps();
    }
}

/// Barre d'applis d'un écran.
pub(super) unsafe extern "system" fn apps_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_ERASEBKGND => LRESULT(1),
        // Le menu a sa propre boucle de messages : on l'ouvre hors de `with_app`.
        WM_RBUTTONUP => {
            dock_menu(hwnd, point(lp));
            LRESULT(0)
        }
        WM_MOUSEMOVE | WM_MOUSELEAVE | WM_LBUTTONDOWN | WM_LBUTTONUP | WM_MOUSEWHEEL => {
            with_app(|app| app.dock_mouse(hwnd, msg, wp, lp));
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// Menu du clic droit sur une appli ou la corbeille (ouvert hors de `with_app` : il a sa
/// propre boucle).
unsafe fn dock_menu(hwnd: HWND, pt: POINT) {
    let Some(Some((index, kind, running, pinned))) = with_app(|app| {
        let s = app.apps_index(hwnd)?;
        let Some(Part::Item(index)) = app.apps_part(s, pt) else { return None };
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
            items.push((MENU_DOCK_PIN, w!("Garder dans la barre")));
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

/// Loupe du lanceur : un anneau et son manche, en blanc.
fn draw_magnifier(canvas: &mut Canvas, r: RECT) {
    // Dessinée pour une case de 20 px ; elle suit la taille de la case (grossie au survol).
    let k = (r.right - r.left) as f32 / 20.0;
    let (cx, cy) = ((r.left + r.right) as f32 / 2.0 - 1.5 * k, (r.top + r.bottom) as f32 / 2.0 - 1.5 * k);
    let white = (255, 255, 255);
    // Anneau : un arc ouvert sur tout le tour.
    canvas.arc((cx, cy), 5.5 * k, PI + 0.1, 1.8 * k, white, 255);
    // Manche, en diagonale vers le bas à droite.
    let (d, t) = (5.5 * k + 0.6 * k, 1.1 * k);
    let (x0, y0) = (cx + d * FRAC_1_SQRT_2, cy + d * FRAC_1_SQRT_2);
    let (x1, y1) = (x0 + 4.5 * k * FRAC_1_SQRT_2, y0 + 4.5 * k * FRAC_1_SQRT_2);
    let (nx, ny) = (t * FRAC_1_SQRT_2, -t * FRAC_1_SQRT_2);
    canvas.fill_convex(&[(x0 + nx, y0 + ny), (x1 + nx, y1 + ny), (x1 - nx, y1 - ny), (x0 - nx, y0 - ny)], white, 255);
}

/// Haut-parleur avec une à trois ondes selon le volume, ou une croix si le son est coupé.
fn draw_volume(canvas: &mut Canvas, button: RECT, level: f32, muted: bool) {
    // Dessiné pour une case de 20 px ; il suit la taille de la case (grossie au survol).
    let k = (button.right - button.left).min(button.bottom - button.top) as f32 / 20.0 * 0.9;
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

/// Largeur d'un texte dans une police donnée, en pixels. Le DC de mesure sert toute la vie du
/// programme : l'encoche mesure ses textes à chaque image de ses animations.
pub(super) fn text_width(font: HFONT, text: &str) -> i32 {
    thread_local! {
        static MEASURE: HDC = unsafe { CreateCompatibleDC(None) };
    }
    MEASURE.with(|&dc| unsafe {
        let old = SelectObject(dc, font);
        let wide: Vec<u16> = text.encode_utf16().collect();
        let mut size = SIZE::default();
        let _ = GetTextExtentPoint32W(dc, &wide, &mut size);
        SelectObject(dc, old);
        size.cx
    })
}
