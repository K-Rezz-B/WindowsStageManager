//! Les contrôles rapides : une pastille noire dans le coin haut droit de chaque écran. Wi-Fi,
//! Bluetooth, son, luminosité, « Ne pas déranger » et batterie, puis la date du jour ; un clic sur
//! une icône ouvre son panneau dessous, comme les menus de la barre du Mac. Un clic droit ouvre la
//! page des Paramètres correspondante.

use std::f32::consts::PI;

use super::{
    activity::{draw_battery, draw_sun, GREEN, LOW, RED},
    *,
};
use crate::{
    brightness,
    canvas::round_corners,
    quick::{Details, Panel, Quick, Toggle},
    shell, volume,
};

pub(super) const QUICK_CLASS: PCWSTR = w!("StageManagerQuick");
/// L'état du Wi-Fi ou du Bluetooth (ou le contenu d'un panneau) a changé.
pub(super) const WM_QUICK: u32 = WM_APP + 12;

/// Fermeture du panneau quand la souris quitte la barre : un léger délai pardonne un écart.
const TIMER_QUICK_CLOSE: usize = 7;
const CLOSE_DELAY: u32 = 450;
const PANEL_OPEN: Duration = Duration::from_millis(220);

/// Panneau : largeur, écart sous la pastille, marges, hauteur de l'en-tête, des lignes et du pied.
const PANEL_W: i32 = 290;
const PANEL_GAP: i32 = 8;
const PAD: i32 = 14;
const HEADER_H: i32 = 46;
const ROW_H: i32 = 34;
const FOOTER_H: i32 = 38;
/// Réseaux ou appareils montrés au plus.
const MAX_ROWS: usize = 8;
/// Interrupteur : largeur, hauteur.
const SWITCH: (i32, i32) = (38, 22);
/// Bleu de macOS : interrupteurs allumés, réseau et appareils connectés.
const ACCENT: (u8, u8, u8) = (10, 132, 255);
const GREY: (u8, u8, u8) = (72, 72, 78);

pub(super) struct QuickUi {
    sys: Quick,
    /// Écran dont la barre est sous la souris (ou porte le panneau ouvert).
    active: Option<ScreenId>,
    tracking: bool,
    /// Élément sous la souris, celui d'avant (le halo en part) et le début du glissement du halo.
    hover: Option<Hit>,
    hover_prev: Option<Hit>,
    hover_start: Option<Instant>,
    /// Panneau ouvert, et le début de son apparition.
    panel: Option<Panel>,
    panel_start: Option<Instant>,
    /// Volume (entre 0 et 1) et son coupé, relus chaque seconde.
    volume: Option<(f32, bool)>,
    /// On fait glisser le curseur de volume.
    drag: bool,
    /// Date du jour (« lun. 3 nov. »), au bout de la pastille.
    date: String,
    /// Police des titres, par échelle d'écran.
    bold: Vec<(i32, HFONT)>,
}

/// Ce qu'il y a sous la souris.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    Icon(Panel),
    Switch,
    /// Un réseau Wi-Fi (rang dans la liste).
    Row(usize),
    Mute,
    Slider,
    Settings,
}

/// Le panneau sous la pastille.
struct PanelLayout {
    rect: RECT,
    title: RECT,
    switch: Option<RECT>,
    /// Lignes : réseaux, appareils ou texte ; pour le son, le curseur puis la sortie audio.
    rows: Vec<RECT>,
    /// Icône au début du curseur : le haut-parleur (qui coupe le son) ou le soleil.
    mute: Option<RECT>,
    track: Option<RECT>,
    /// Zone cliquable de la piste, plus haute que la piste elle-même.
    track_hit: Option<RECT>,
    divider: i32,
    settings: RECT,
}

/// Emplacement des éléments, en coordonnées de la fenêtre de la barre.
struct QuickLayout {
    size: SIZE,
    pill: RECT,
    icons: Vec<(Panel, RECT)>,
    date: RECT,
    panel: Option<PanelLayout>,
}

impl QuickUi {
    /// `hwnd` : la fenêtre principale, prévenue quand l'état change.
    pub fn new(hwnd: HWND) -> Self {
        Self {
            sys: Quick::start(hwnd, WM_QUICK),
            active: None,
            tracking: false,
            hover: None,
            hover_prev: None,
            hover_start: None,
            panel: None,
            panel_start: None,
            volume: None,
            drag: false,
            date: dockbar::clock_text().1,
            bold: Vec::new(),
        }
    }

    /// Avancement du halo du survol depuis son dernier changement, en douceur.
    fn hover_progress(&self, now: Instant) -> f64 {
        anim::ease_out(self.hover_start.map_or(1.0, |start| anim::progress(start, HOVER_ANIM, now)))
    }
}

/// Le contrôle que bascule l'interrupteur d'un panneau.
fn toggle_of(panel: Panel) -> Option<Toggle> {
    match panel {
        Panel::Wifi => Some(Toggle::Wifi),
        Panel::Bluetooth => Some(Toggle::Bluetooth),
        Panel::Focus => Some(Toggle::Focus),
        Panel::Sound | Panel::Brightness | Panel::Battery => None,
    }
}

fn title_of(panel: Panel) -> &'static str {
    match panel {
        Panel::Wifi => "Wi-Fi",
        Panel::Bluetooth => "Bluetooth",
        Panel::Sound => "Son",
        Panel::Brightness => "Luminosité",
        Panel::Focus => "Ne pas déranger",
        Panel::Battery => "Batterie",
    }
}

fn settings_of(panel: Panel) -> (&'static str, &'static str) {
    match panel {
        Panel::Wifi => ("Paramètres Wi-Fi…", "ms-settings:network-wifi"),
        Panel::Bluetooth => ("Paramètres Bluetooth…", "ms-settings:bluetooth"),
        Panel::Sound => ("Paramètres du son…", "ms-settings:sound"),
        Panel::Brightness => ("Paramètres d'affichage…", "ms-settings:display"),
        Panel::Focus => ("Paramètres des notifications…", "ms-settings:notifications"),
        Panel::Battery => ("Paramètres de la batterie…", "ms-settings:batterysaver"),
    }
}

/// Le panneau a un curseur : le volume ou la luminosité.
fn has_slider(panel: Panel) -> bool {
    matches!(panel, Panel::Sound | Panel::Brightness)
}

impl App {
    pub(super) fn handle_quick_message(&mut self, msg: u32) -> bool {
        if msg != WM_QUICK {
            return false;
        }
        self.render_all_quick();
        true
    }

    /// Chaque seconde : le volume, qui a pu changer au clavier, et la date, à minuit.
    pub(super) fn quick_clock(&mut self) {
        let volume = volume::get();
        let date = dockbar::clock_text().1;
        if volume != self.quick.volume || date != self.quick.date {
            self.quick.volume = volume;
            self.quick.date = date;
            self.render_all_quick();
        }
    }

    /// Les icônes de la pastille : celles que l'appareil n'a pas (pas de Wi-Fi sur un PC fixe,
    /// pas de batterie…) n'apparaissent pas.
    fn quick_panels(&self) -> Vec<Panel> {
        let state = self.quick.sys.state();
        let mut panels = Vec::new();
        if state.wifi.is_some() {
            panels.push(Panel::Wifi);
        }
        if state.bluetooth.is_some() {
            panels.push(Panel::Bluetooth);
        }
        panels.push(Panel::Sound);
        if self.activity.brightness.is_some() {
            panels.push(Panel::Brightness);
        }
        if state.focus.is_some() {
            panels.push(Panel::Focus);
        }
        if self.activity.battery.is_some() {
            panels.push(Panel::Battery);
        }
        panels
    }

    /// Niveau du curseur d'un panneau, entre 0 et 1.
    fn slider_level(&self, panel: Panel) -> f32 {
        match panel {
            Panel::Brightness => self.activity.brightness.unwrap_or(0).min(100) as f32 / 100.0,
            _ => self.quick.volume.map_or(0.0, |(level, _)| level),
        }
    }

    /// Règle le volume ou la luminosité. La luminosité affichée suit tout de suite, sans
    /// attendre l'avis de Windows.
    fn set_slider(&mut self, panel: Panel, level: f32) {
        let level = level.clamp(0.0, 1.0);
        if panel == Panel::Brightness {
            let percent = (level * 100.0).round() as u32;
            self.activity.brightness = Some(percent);
            brightness::set(percent);
        } else {
            volume::set(level);
            self.quick.volume = volume::get();
        }
    }

    pub(super) fn quick_index(&self, hwnd: HWND) -> Option<usize> {
        self.screens.iter().position(|s| s.quick == hwnd)
    }

    // --- Disposition --------------------------------------------------------------------

    fn quick_layout(&self, s: usize) -> QuickLayout {
        let px = |v: i32| self.px(s, v);
        let (icon, gap) = (px(ICON), px(CORNER_GAP));
        let panels = self.quick_panels();
        let n = panels.len() as i32;
        let shown = self.quick.active == Some(self.screens[s].id);
        let open = self.quick.panel.filter(|_| shown);
        // La date suit les icônes.
        let date_w = dockbar::text_width(self.screens[s].font, &self.quick.date);
        // Le panneau s'ouvre vers la gauche, aligné sur le bord droit de la pastille.
        let pill_w = 2 * px(PILL_PAD) + n * icon + (n - 1) * gap + gap + date_w;
        let left = px(PILL_SHOULDER) + px(14) + open.map_or(0, |_| (px(PANEL_W) - pill_w).max(0));
        let pill = RECT { left, top: 0, right: left + pill_w, bottom: px(BAR_H) };
        let top = (pill.bottom - icon) / 2;
        let icons = panels
            .iter()
            .enumerate()
            .map(|(i, &p)| {
                let x = pill.left + px(PILL_PAD) + i as i32 * (icon + gap);
                (p, RECT { left: x, top, right: x + icon, bottom: top + icon })
            })
            .collect();
        let date = RECT { left: pill.right - px(PILL_PAD) - date_w, top: pill.top, right: pill.right - px(PILL_PAD), bottom: pill.bottom };
        let panel = open.map(|p| self.panel_layout(s, p, pill));
        let bottom = panel.as_ref().map_or(pill.bottom + px(8), |p| p.rect.bottom + px(14));
        QuickLayout { size: SIZE { cx: pill.right + px(EDGE_GAP), cy: bottom }, pill, icons, date, panel }
    }

    fn panel_layout(&self, s: usize, panel: Panel, pill: RECT) -> PanelLayout {
        let px = |v: i32| self.px(s, v);
        let state = self.quick.sys.state();
        let details = self.quick.sys.details();
        let top = pill.bottom + px(PANEL_GAP);
        let (left, right) = (pill.right - px(PANEL_W), pill.right);
        let title = RECT { left: left + px(PAD), top, right: right - px(PAD + SWITCH.0 + 8), bottom: top + px(HEADER_H) };
        let switch = toggle_of(panel).map(|_| {
            let cy = (title.top + title.bottom) / 2;
            RECT { left: right - px(PAD + SWITCH.0), top: cy - px(SWITCH.1) / 2, right: right - px(PAD), bottom: cy + px(SWITCH.1) / 2 }
        });
        let count = match panel {
            Panel::Wifi if state.wifi == Some(true) => details.networks.len().clamp(1, MAX_ROWS),
            Panel::Bluetooth if state.bluetooth == Some(true) => details.devices.len().clamp(1, MAX_ROWS),
            Panel::Sound | Panel::Battery => 2,
            _ => 1,
        };
        let rows: Vec<RECT> = (0..count as i32)
            .map(|i| {
                let y = title.bottom + i * px(ROW_H);
                RECT { left: left + px(6), top: y, right: right - px(6), bottom: y + px(ROW_H) }
            })
            .collect();
        let (mut mute, mut track, mut track_hit) = (None, None, None);
        if has_slider(panel) {
            let row = rows[0];
            let cy = (row.top + row.bottom) / 2;
            let m = RECT { left: row.left + px(4), top: cy - px(14), right: row.left + px(32), bottom: cy + px(14) };
            let t = RECT { left: m.right + px(10), top: cy - px(3), right: row.right - px(48), bottom: cy + px(3) };
            mute = Some(m);
            track = Some(t);
            track_hit = Some(RECT { left: t.left - px(8), top: row.top, right: t.right + px(8), bottom: row.bottom });
        }
        let last = rows.last().map_or(title.bottom, |r| r.bottom);
        let divider = last + px(6);
        let settings = RECT { left: left + px(6), top: divider + px(5), right: right - px(6), bottom: divider + px(5) + px(FOOTER_H - 10) };
        let rect = RECT { left, top, right, bottom: settings.bottom + px(6) };
        PanelLayout { rect, title, switch, rows, mute, track, track_hit, divider, settings }
    }

    /// Position de la fenêtre : la pastille à `EDGE_GAP` du bord droit de l'écran, face à la
    /// barre d'applis.
    fn quick_window(&self, s: usize, size: SIZE) -> RECT {
        let sc = &self.screens[s];
        let right = sc.rect.right;
        RECT { left: right - size.cx, top: sc.rect.top, right, bottom: sc.rect.top + size.cy }
    }

    // --- Mise en place ------------------------------------------------------------------

    /// Montre (ou cache) les contrôles rapides d'un écran, avec la barre d'applis.
    pub(super) fn place_quick(&mut self, s: usize) {
        let sc = &self.screens[s];
        let shown = self.dock.enabled && !sc.fullscreen;
        unsafe {
            let _ = ShowWindow(sc.quick, if shown { SW_SHOWNOACTIVATE } else { SW_HIDE });
        }
        if !self.dock.enabled {
            self.close_panel();
        }
        if shown {
            self.render_quick(s);
        }
    }

    // --- Dessin -----------------------------------------------------------------------

    pub(super) fn render_all_quick(&mut self) {
        for s in 0..self.screens.len() {
            self.render_quick(s);
        }
    }

    fn bold_font(&mut self, dpi: i32) -> HFONT {
        if let Some(&(_, font)) = self.quick.bold.iter().find(|(d, _)| *d == dpi) {
            return font;
        }
        let font = unsafe { make_font("Inter SemiBold", 13, 600, dpi) };
        self.quick.bold.push((dpi, font));
        font
    }

    pub(super) fn render_quick(&mut self, s: usize) {
        if !self.dock.enabled || self.screens[s].fullscreen {
            return;
        }
        let bold = self.bold_font(self.screens[s].dpi);
        let now = anim::now();
        let layout = self.quick_layout(s);
        let window = self.quick_window(s, layout.size);
        let sc = &self.screens[s];
        let px = |v: i32| self.px(s, v);
        let (w, h) = (layout.size.cx, layout.size.cy);
        let mut canvas = Canvas::new(w, h);

        // Rentrée dans le bord de l'écran pendant que l'encoche est ouverte, comme les scènes.
        let shown = self.bar_shown(s, now);
        if shown <= 0.0 {
            canvas.present(sc.quick, POINT { x: window.left, y: window.top });
            return;
        }
        let active = self.quick.active == Some(sc.id);
        let lift = ((1.0 - shown) * px(BAR_H + 4) as f64).round() as i32;
        let up = |r: RECT| offset(r, 0, -lift);
        let mut pill = Canvas::new(w, h);
        pill.notch(up(layout.pill), px(PILL_RADIUS) as f32, px(PILL_SHOULDER) as f32, (0, 0, 0), 255);

        // Les icônes ; éteintes, elles sont dessinées à part puis estompées.
        let state = self.quick.sys.state();
        let (level, muted) = self.quick.volume.unwrap_or((0.0, false));
        let battery = self.activity.battery.unwrap_or(0);
        let charging = self.activity.on_ac == Some(true) && battery < 100;
        let mut dimmed = Canvas::new(w, h);
        // Rangée des icônes, pour le halo du survol.
        let rank = |hit: Option<Hit>| match hit {
            Some(Hit::Icon(p)) => layout.icons.iter().position(|(q, _)| *q == p),
            _ => None,
        };
        let (hover, prev) = (rank(self.quick.hover), rank(self.quick.hover_prev));
        let t = self.quick.hover_progress(now);
        if active {
            let rects: Vec<RECT> = layout.icons.iter().map(|&(_, r)| up(inflate(r, px(5)))).collect();
            if let Some((halo, alpha)) = hover_halo(&rects, hover, prev, t) {
                pill.round_rect(halo, px(7), (255, 255, 255), (HALO_ALPHA * alpha.clamp(0.0, 1.0)).round() as u8);
            }
        }
        for &(p, r) in &layout.icons {
            if active && self.quick.panel == Some(p) {
                // Le panneau de cette icône est ouvert : un fond clair la signale.
                let back = up(inflate(r, px(5)));
                pill.round_rect(back, px(7), (255, 255, 255), 45);
            }
            let r = up(r);
            let on = match p {
                Panel::Wifi => state.wifi == Some(true),
                Panel::Bluetooth => state.bluetooth == Some(true),
                Panel::Focus => state.focus == Some(true),
                Panel::Sound | Panel::Brightness | Panel::Battery => true,
            };
            let target = if on { &mut pill } else { &mut dimmed };
            let k = (r.right - r.left) as f32 / 20.0;
            match p {
                Panel::Wifi => draw_wifi(target, r, 3),
                Panel::Bluetooth => draw_bluetooth(target, r),
                Panel::Sound => draw_volume(target, r, level, muted),
                Panel::Brightness => draw_sun(target, r, k * 0.95),
                Panel::Focus => draw_moon(target, r),
                Panel::Battery => draw_battery(target, r, battery, battery_color(battery, charging), charging, k),
            }
        }
        pill.overlay(&dimmed, 0.38);
        let (font, date, date_rect) = (sc.font, self.quick.date.clone(), up(layout.date));
        pill.gdi(|dc| unsafe {
            SetBkMode(dc, TRANSPARENT);
            SelectObject(dc, font);
            draw_text(dc, &date, date_rect, DT_RIGHT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX, TEXT, 0);
        });
        canvas.overlay(&pill, shown as f32);

        if let (Some(p), Some(pl)) = (self.quick.panel, layout.panel.as_ref()) {
            if shown >= 1.0 {
                self.draw_panel(&mut canvas, &layout, pl, p, s, bold, now);
            }
        }
        let sc = &self.screens[s];
        canvas.present(sc.quick, POINT { x: window.left, y: window.top });
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_panel(&self, canvas: &mut Canvas, layout: &QuickLayout, pl: &PanelLayout, panel: Panel, s: usize, bold: HFONT, now: Instant) {
        let px = |v: i32| self.px(s, v);
        let font = self.screens[s].font;
        let (w, h) = (layout.size.cx, layout.size.cy);
        let white = (255, 255, 255);
        let state = self.quick.sys.state();
        let details: Details = self.quick.sys.details();
        let hover = self.quick.hover;
        let appear = self.quick.panel_start.map_or(1.0, |start| anim::ease_out(anim::progress(start, PANEL_OPEN, now)) as f32);
        let look = Material { tint: (30, 30, 32), opacity: 0.96, border: 0.14 };

        // Zone invisible mais « pleine » entre la pastille et le panneau : la souris passe de l'un
        // à l'autre sans quitter la barre.
        canvas.round_rect(RECT { top: layout.pill.bottom - px(2), bottom: pl.rect.top + px(2), ..pl.rect }, 0, (0, 0, 0), 1);

        let mut layer = Canvas::new(w, h);
        // Le panneau descend un peu en apparaissant.
        let drop = ((1.0 - appear) * px(6) as f32).round() as i32;
        let r = |rect: RECT| offset(rect, 0, -drop);
        layer.round_rect(offset(inflate(r(pl.rect), px(1)), 0, px(3)), px(15), (0, 0, 0), 45);
        layer.material(r(pl.rect), px(14) as f32, None, (0, 0), &look);

        let on = match panel {
            Panel::Wifi => state.wifi == Some(true),
            Panel::Bluetooth => state.bluetooth == Some(true),
            Panel::Focus => state.focus == Some(true),
            Panel::Sound | Panel::Brightness | Panel::Battery => true,
        };
        if let Some(sw) = pl.switch {
            draw_switch(&mut layer, r(sw), on);
        }
        // Survol des lignes cliquables.
        let highlight = |layer: &mut Canvas, rect: RECT| layer.round_rect(r(rect), px(7), white, 28);
        if hover == Some(Hit::Settings) {
            highlight(&mut layer, pl.settings);
        }
        let divider = RECT { left: pl.rect.left + px(PAD), top: pl.divider - drop, right: pl.rect.right - px(PAD), bottom: pl.divider - drop + px(1).max(1) };
        layer.round_rect(divider, 0, white, 40);

        // Lignes : les cercles et icônes d'abord, les textes ensuite (en une seule passe GDI).
        let mut texts: Vec<(String, RECT, HFONT, COLORREF, DRAW_TEXT_FORMAT)> = Vec::new();
        let line = DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX;
        let right = DT_RIGHT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX;
        let circle = |row: RECT| {
            let d = px(26);
            let cy = (row.top + row.bottom) / 2;
            RECT { left: row.left + px(8), top: cy - d / 2, right: row.left + px(8) + d, bottom: cy + d / 2 }
        };
        let label = |row: RECT, right_space: i32| RECT { left: row.left + px(44), right: row.right - px(right_space), ..row };
        let message = |text: &str, row: RECT| (text.to_owned(), RECT { left: row.left + px(8), ..row }, font, TEXT_DIM, line);
        match panel {
            Panel::Wifi if !on => texts.push(message("Le Wi-Fi est désactivé.", pl.rows[0])),
            Panel::Wifi if details.networks.is_empty() => texts.push(message("Recherche des réseaux…", pl.rows[0])),
            Panel::Wifi => {
                for (i, (n, &row)) in details.networks.iter().zip(&pl.rows).enumerate() {
                    if hover == Some(Hit::Row(i)) && !n.connected {
                        highlight(&mut layer, row);
                    }
                    let c = r(circle(row));
                    layer.round_rect(c, (c.right - c.left) / 2, if n.connected { ACCENT } else { GREY }, 255);
                    let bars = if n.signal >= 67 { 3 } else if n.signal >= 34 { 2 } else { 1 };
                    draw_wifi(&mut layer, inflate(c, -px(5)), bars);
                    if n.secured {
                        let cy = (row.top + row.bottom) / 2 - drop;
                        draw_lock(&mut layer, ((row.right - px(16)) as f32, cy as f32), px(1) as f32);
                    }
                    let f = if n.connected { bold } else { font };
                    texts.push((n.ssid.clone(), r(label(row, 32)), f, TEXT, line));
                }
            }
            Panel::Bluetooth if !on => texts.push(message("Le Bluetooth est désactivé.", pl.rows[0])),
            Panel::Bluetooth if details.devices.is_empty() => texts.push(message("Aucun appareil associé.", pl.rows[0])),
            Panel::Bluetooth => {
                for (d, &row) in details.devices.iter().zip(&pl.rows) {
                    let c = r(circle(row));
                    layer.round_rect(c, (c.right - c.left) / 2, if d.connected { ACCENT } else { GREY }, 255);
                    draw_bluetooth(&mut layer, inflate(c, -px(5)));
                    texts.push((d.name.clone(), r(label(row, 80)), font, TEXT, line));
                    if d.connected {
                        texts.push(("Connecté".to_owned(), r(RECT { left: row.right - px(80), right: row.right - px(8), ..row }), font, TEXT_DIM, right));
                    }
                }
            }
            Panel::Sound | Panel::Brightness => {
                let level = self.slider_level(panel);
                let muted = panel == Panel::Sound && self.quick.volume.is_some_and(|(_, m)| m);
                let (m, t) = (pl.mute.unwrap_or_default(), pl.track.unwrap_or_default());
                if hover == Some(Hit::Mute) {
                    layer.round_rect(r(m), (m.right - m.left) / 2, white, 40);
                }
                let icon = r(inflate(m, -px(4)));
                if panel == Panel::Sound {
                    draw_volume(&mut layer, icon, level, muted);
                } else {
                    draw_sun(&mut layer, icon, (icon.right - icon.left) as f32 / 20.0);
                }
                let t = r(t);
                let radius = (t.bottom - t.top) / 2;
                layer.round_rect(t, radius, white, 60);
                let x = t.left + ((t.right - t.left) as f32 * level).round() as i32;
                if x > t.left {
                    layer.round_rect(RECT { right: x.max(t.left + 2 * radius), ..t }, radius, white, if muted { 120 } else { 235 });
                }
                // Bouton rond, avec une ombre douce.
                let knob = px(16);
                let cy = (t.top + t.bottom) / 2;
                let k = RECT { left: x - knob / 2, top: cy - knob / 2, right: x + knob / 2, bottom: cy + knob / 2 };
                layer.round_rect(offset(inflate(k, px(1)), 0, px(1)), knob / 2 + px(1), (0, 0, 0), 70);
                layer.round_rect(k, knob / 2, white, 255);
                let percent = if muted { "—".to_owned() } else { format!("{} %", (level * 100.0).round()) };
                let row = pl.rows[0];
                texts.push((percent, r(RECT { left: row.right - px(46), right: row.right - px(6), ..row }), font, TEXT, right));
                if panel == Panel::Sound {
                    let output = match (&details.output, self.quick.volume) {
                        (_, None) => "Aucune sortie audio".to_owned(),
                        (Some(name), _) => format!("Sortie : {name}"),
                        (None, _) => String::new(),
                    };
                    texts.push(message(&output, pl.rows[1]));
                }
            }
            Panel::Focus => {
                let text = if on { "Les notifications sont masquées." } else { "Les notifications s'affichent." };
                texts.push(message(text, pl.rows[0]));
            }
            Panel::Battery => {
                let percent = self.activity.battery.unwrap_or(0);
                let on_ac = self.activity.on_ac == Some(true);
                let charging = on_ac && percent < 100;
                let row = pl.rows[0];
                let cy = (row.top + row.bottom) / 2;
                let icon = r(RECT { left: row.left + px(6), top: cy - px(13), right: row.left + px(34), bottom: cy + px(13) });
                draw_battery(&mut layer, icon, percent, battery_color(percent, charging), charging, (icon.right - icon.left) as f32 / 20.0);
                texts.push((format!("{percent} %"), r(label(row, 110)), bold, TEXT, line));
                let source = if on_ac { "Sur secteur" } else { "Sur batterie" };
                texts.push((source.to_owned(), r(RECT { left: row.right - px(110), right: row.right - px(8), ..row }), font, TEXT_DIM, right));
                texts.push(message(&battery_detail(percent, on_ac), pl.rows[1]));
            }
        }
        let (settings_text, _) = settings_of(panel);
        let title = title_of(panel);
        let (title_rect, settings_rect) = (r(pl.title), r(RECT { left: pl.settings.left + px(8), ..pl.settings }));
        layer.gdi(|dc| unsafe {
            SetBkMode(dc, TRANSPARENT);
            SelectObject(dc, bold);
            draw_text(dc, title, title_rect, line, TEXT, 0);
            for (text, rect, f, color, flags) in &texts {
                SelectObject(dc, *f);
                draw_text(dc, text, *rect, *flags, *color, 0);
            }
            SelectObject(dc, font);
            draw_text(dc, settings_text, settings_rect, line, TEXT, 0);
        });
        canvas.overlay(&layer, appear);
    }

    pub(super) fn quick_animating(&self) -> bool {
        self.dock.enabled && (self.quick.hover_start.is_some() || self.quick.panel_start.is_some())
    }

    /// Avance les animations des contrôles rapides. Renvoie `true` s'il faut les redessiner.
    pub(super) fn quick_tick(&mut self, now: Instant) -> bool {
        if !self.dock.enabled {
            return false;
        }
        let mut redraw = false;
        if let Some(start) = self.quick.hover_start {
            if anim::progress(start, HOVER_ANIM, now) >= 1.0 {
                self.quick.hover_start = None;
            }
            redraw = true;
        }
        if let Some(start) = self.quick.panel_start {
            if anim::progress(start, PANEL_OPEN, now) >= 1.0 {
                self.quick.panel_start = None;
            }
            redraw = true;
        }
        redraw
    }

    // --- Panneau ------------------------------------------------------------------------

    fn open_panel(&mut self, panel: Panel) {
        self.quick.panel = Some(panel);
        self.quick.panel_start = Some(Instant::now());
        self.quick.volume = volume::get();
        self.quick.sys.watch(Some(panel));
        // Le panneau passe devant la barre des scènes, qu'il recouvre en partie.
        for sc in &self.screens {
            unsafe {
                let _ = SetWindowPos(sc.quick, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            }
        }
        self.render_all_quick();
    }

    fn close_panel(&mut self) {
        if self.quick.panel.take().is_some() {
            self.quick.panel_start = None;
            self.quick.sys.watch(None);
            self.render_all_quick();
        }
    }

    // --- Souris -----------------------------------------------------------------------

    fn quick_hit(&self, s: usize, pt: POINT) -> Option<Hit> {
        let layout = self.quick_layout(s);
        let half_gap = self.px(s, CORNER_GAP) / 2;
        let cell = |r: RECT| RECT { left: r.left - half_gap, top: layout.pill.top, right: r.right + half_gap, bottom: layout.pill.bottom };
        if let Some(&(p, _)) = layout.icons.iter().find(|(_, r)| contains(cell(*r), pt)) {
            return Some(Hit::Icon(p));
        }
        let pl = layout.panel.as_ref()?;
        let panel = self.quick.panel?;
        if pl.switch.is_some_and(|r| contains(inflate(r, self.px(s, 4)), pt)) {
            return Some(Hit::Switch);
        }
        if panel == Panel::Sound && pl.mute.is_some_and(|r| contains(r, pt)) {
            return Some(Hit::Mute);
        }
        if pl.track_hit.is_some_and(|r| contains(r, pt)) {
            return Some(Hit::Slider);
        }
        if contains(pl.settings, pt) {
            return Some(Hit::Settings);
        }
        if panel == Panel::Wifi && self.quick.sys.state().wifi == Some(true) {
            let networks = self.quick.sys.details().networks.len();
            if let Some(i) = pl.rows.iter().take(networks).position(|r| contains(*r, pt)) {
                return Some(Hit::Row(i));
            }
        }
        None
    }

    fn set_quick_hover(&mut self, hover: Option<Hit>) {
        if self.quick.hover != hover {
            self.quick.hover_prev = self.quick.hover;
            self.quick.hover = hover;
            self.quick.hover_start = Some(Instant::now());
        }
    }

    pub(super) fn quick_timer(&mut self, hwnd: HWND, timer: usize) {
        if timer != TIMER_QUICK_CLOSE {
            return;
        }
        unsafe {
            let _ = KillTimer(hwnd, TIMER_QUICK_CLOSE);
        }
        if !self.quick.tracking && !self.quick.drag {
            self.close_panel();
        }
    }

    /// Souris sur les contrôles rapides de la fenêtre `hwnd`.
    pub(super) fn quick_mouse(&mut self, hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) {
        let Some(s) = self.quick_index(hwnd) else { return };
        let pt = point(lp);
        let id = self.screens[s].id;
        match msg {
            WM_MOUSEMOVE => {
                unsafe {
                    let _ = KillTimer(hwnd, TIMER_QUICK_CLOSE);
                }
                if self.quick.active != Some(id) {
                    // La souris passe d'un écran à l'autre : le panneau reste sur le premier.
                    let previous = self.quick.active.replace(id).and_then(|p| self.screen_index(p));
                    self.quick.tracking = false;
                    if let Some(p) = previous {
                        self.close_panel();
                        self.render_quick(p);
                    }
                }
                if !self.quick.tracking {
                    let mut tme = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                    self.quick.tracking = unsafe { TrackMouseEvent(&mut tme) }.is_ok();
                }
                if self.quick.drag {
                    self.drag_quick_slider(s, pt.x);
                    return;
                }
                let hit = self.quick_hit(s, pt);
                if hit != self.quick.hover {
                    self.set_quick_hover(hit);
                    self.render_quick(s);
                }
            }
            WM_MOUSELEAVE => {
                self.quick.tracking = false;
                self.set_quick_hover(None);
                if self.quick.panel.is_some() {
                    unsafe {
                        SetTimer(hwnd, TIMER_QUICK_CLOSE, CLOSE_DELAY, None);
                    }
                }
                self.render_quick(s);
            }
            WM_LBUTTONDOWN => {
                if self.quick_hit(s, pt) == Some(Hit::Slider) {
                    self.quick.drag = true;
                    unsafe {
                        // Garde la souris même si elle sort de la piste en glissant.
                        SetCapture(hwnd);
                    }
                    self.drag_quick_slider(s, pt.x);
                }
            }
            WM_LBUTTONUP if self.quick.drag => {
                self.quick.drag = false;
                unsafe {
                    let _ = ReleaseCapture();
                }
            }
            WM_LBUTTONUP => self.quick_click(s, pt),
            WM_RBUTTONUP => {
                // Clic droit sur une icône : sa page des Paramètres.
                if let Some(Hit::Icon(p)) = self.quick_hit(s, pt) {
                    self.close_panel();
                    shell::open(settings_of(p).1);
                }
            }
            WM_MOUSEWHEEL => {
                // La molette au-dessus du son ou de la luminosité (icône ou curseur) les règle, par
                // pas de 2 %. (Pour cette molette, Windows donne la position à l'écran.)
                let win = self.quick_window(s, self.quick_layout(s).size);
                let local = POINT { x: pt.x - win.left, y: pt.y - win.top };
                let target = match self.quick_hit(s, local) {
                    Some(Hit::Icon(p)) if has_slider(p) => Some(p),
                    Some(Hit::Mute | Hit::Slider) => self.quick.panel,
                    _ => None,
                };
                if let Some(p) = target {
                    let notches = ((wp.0 >> 16) & 0xFFFF) as i16 as f32 / 120.0;
                    if p == Panel::Sound {
                        self.quick.volume = volume::get();
                    }
                    let level = self.slider_level(p);
                    self.set_slider(p, level + notches * 0.02);
                    self.render_quick(s);
                }
            }
            _ => {}
        }
    }

    fn quick_click(&mut self, s: usize, pt: POINT) {
        let Some(hit) = self.quick_hit(s, pt) else { return };
        let panel = self.quick.panel;
        match hit {
            Hit::Icon(p) if panel == Some(p) => self.close_panel(),
            Hit::Icon(p) => self.open_panel(p),
            Hit::Switch => {
                if let Some(t) = panel.and_then(toggle_of) {
                    self.quick.sys.toggle(t);
                    self.render_quick(s);
                }
            }
            Hit::Row(i) => {
                let Some(network) = self.quick.sys.details().networks.get(i).cloned() else { return };
                if network.connected {
                    return;
                }
                if network.profile.is_empty() {
                    // Réseau inconnu : le volet réseau de Windows demande le mot de passe.
                    self.close_panel();
                    shell::open("ms-availablenetworks:");
                } else {
                    self.quick.sys.connect(&network.profile);
                }
            }
            Hit::Mute => {
                volume::toggle_mute();
                self.quick.volume = volume::get();
                self.render_quick(s);
            }
            Hit::Slider => {}
            Hit::Settings => {
                if let Some(p) = panel {
                    self.close_panel();
                    shell::open(settings_of(p).1);
                }
            }
        }
    }

    /// Règle le volume ou la luminosité d'après la position de la souris sur la piste du curseur.
    fn drag_quick_slider(&mut self, s: usize, x: i32) {
        let Some(panel) = self.quick.panel else { return };
        let Some(track) = self.quick_layout(s).panel.and_then(|p| p.track) else { return };
        let level = (x - track.left) as f32 / (track.right - track.left).max(1) as f32;
        self.set_slider(panel, level);
        self.render_quick(s);
    }
}

/// Contrôles rapides d'un écran.
pub(super) unsafe extern "system" fn quick_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_ERASEBKGND => LRESULT(1),
        WM_MOUSEMOVE | WM_MOUSELEAVE | WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONUP | WM_MOUSEWHEEL => {
            with_app(|app| app.quick_mouse(hwnd, msg, wp, lp));
            LRESULT(0)
        }
        WM_TIMER => {
            with_app(|app| app.quick_timer(hwnd, wp.0));
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// Interrupteur façon macOS : bleu et bouton à droite quand il est allumé.
fn draw_switch(canvas: &mut Canvas, r: RECT, on: bool) {
    let h = r.bottom - r.top;
    canvas.round_rect(r, h / 2, if on { ACCENT } else { GREY }, 255);
    let inset = (h / 11).max(1);
    let d = h - 2 * inset;
    let left = if on { r.right - inset - d } else { r.left + inset };
    let knob = RECT { left, top: r.top + inset, right: left + d, bottom: r.top + inset + d };
    canvas.round_rect(offset(knob, 0, 1), d / 2, (0, 0, 0), 50);
    canvas.round_rect(knob, d / 2, (255, 255, 255), 255);
}

/// Wi-Fi : un point et trois ondes en éventail, vers le haut ; `bars` ondes allumées (le
/// signal), les autres discrètes.
fn draw_wifi(canvas: &mut Canvas, r: RECT, bars: usize) {
    let k = (r.right - r.left).min(r.bottom - r.top) as f32 / 20.0;
    let white = (255, 255, 255);
    let (cx, cy) = ((r.left + r.right) as f32 / 2.0, (r.top + r.bottom) as f32 / 2.0 + 6.0 * k);
    let dot = 1.7 * k;
    let d = RECT { left: (cx - dot).round() as i32, top: (cy - dot).round() as i32, right: (cx + dot).round() as i32, bottom: (cy + dot).round() as i32 };
    canvas.round_rect(d, dot.round() as i32, white, 255);
    for (i, radius) in [5.5, 9.5, 13.5].into_iter().enumerate() {
        let alpha = if i < bars { 255 } else { 70 };
        canvas.arc_toward((cx, cy), radius * k, -PI / 2.0, 0.75, 1.9 * k, white, alpha);
    }
}

/// Bluetooth : la rune, un trait vertical et deux chevrons à droite.
pub(super) fn draw_bluetooth(canvas: &mut Canvas, r: RECT) {
    let k = (r.right - r.left).min(r.bottom - r.top) as f32 / 20.0;
    let (cx, cy) = ((r.left + r.right) as f32 / 2.0 - 0.5 * k, (r.top + r.bottom) as f32 / 2.0);
    let p = |x: f32, y: f32| (cx + x * k, cy + y * k);
    let path = [p(-4.5, -4.5), p(4.5, 4.0), p(0.0, 8.5), p(0.0, -8.5), p(4.5, -4.0), p(-4.5, 4.5)];
    for pair in path.windows(2) {
        line(canvas, pair[0], pair[1], 0.9 * k, (255, 255, 255), 255);
    }
}

/// Petit cadenas des réseaux protégés, centré en `(cx, cy)`.
fn draw_lock(canvas: &mut Canvas, (cx, cy): (f32, f32), k: f32) {
    let grey = (200, 200, 208);
    let body = RECT { left: (cx - 4.5 * k) as i32, top: (cy - 0.5 * k) as i32, right: (cx + 4.5 * k) as i32, bottom: (cy + 6.0 * k) as i32 };
    canvas.round_rect(body, (1.5 * k) as i32, grey, 255);
    canvas.arc_toward((cx, cy - 1.0 * k), 3.0 * k, -PI / 2.0, PI / 2.0 + 0.1, 1.4 * k, grey, 255);
}

/// Haut-parleur avec une à trois ondes selon le volume, ou une croix si le son est coupé.
pub(super) fn draw_volume(canvas: &mut Canvas, button: RECT, level: f32, muted: bool) {
    // Dessiné pour une case de 20 px ; il suit la taille de la case.
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

/// Trait épais de `a` à `b` (demi-épaisseur `t`).
pub(super) fn line(canvas: &mut Canvas, a: (f32, f32), b: (f32, f32), t: f32, color: (u8, u8, u8), alpha: u8) {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len = dx.hypot(dy).max(0.001);
    let (nx, ny) = (-dy / len * t, dx / len * t);
    canvas.fill_convex(&[(a.0 + nx, a.1 + ny), (b.0 + nx, b.1 + ny), (b.0 - nx, b.1 - ny), (a.0 - nx, a.1 - ny)], color, alpha);
}

/// Couleur de la charge : verte en charge, rouge quand la batterie est presque vide.
fn battery_color(percent: u32, charging: bool) -> (u8, u8, u8) {
    if charging {
        GREEN
    } else if percent <= LOW[1] {
        RED
    } else {
        (255, 255, 255)
    }
}

/// Seconde ligne du panneau de la batterie : le temps qui reste, d'après Windows.
fn battery_detail(percent: u32, on_ac: bool) -> String {
    use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
    if on_ac {
        return if percent >= 100 { "Batterie chargée".to_owned() } else { "En charge".to_owned() };
    }
    let mut status = SYSTEM_POWER_STATUS::default();
    let seconds = if unsafe { GetSystemPowerStatus(&mut status) }.is_ok() { status.BatteryLifeTime } else { u32::MAX };
    if seconds == u32::MAX {
        return "Calcul du temps restant…".to_owned();
    }
    let minutes = seconds / 60;
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("Environ {m} min restantes"),
        (h, m) => format!("Environ {h} h {m:02} restantes"),
    }
}

/// Lune de « Ne pas déranger » : un croissant, creusé en haut à droite.
fn draw_moon(canvas: &mut Canvas, r: RECT) {
    let k = (r.right - r.left).min(r.bottom - r.top) as f32 / 20.0;
    let (cx, cy) = ((r.left + r.right) as f32 / 2.0, (r.top + r.bottom) as f32 / 2.0);
    canvas.crescent((cx, cy), 7.5 * k, (cx + 4.2 * k, cy - 3.0 * k), 6.2 * k, (255, 255, 255), 255);
}
