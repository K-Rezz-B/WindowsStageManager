//! Fenêtre des réglages, à la manière de macOS : une ligne par fonctionnalité, avec son
//! interrupteur. Elle s'ouvre depuis l'icône de notification, le clic droit sur l'encoche, ou en
//! relançant l'application.

use super::*;
use crate::settings::Feature;

pub(super) const WM_SHOW_SETTINGS: u32 = WM_APP + 11;
const TIMER_SETTINGS: usize = 8;
/// Bascule d'un interrupteur.
const SWITCH_ANIM: Duration = Duration::from_millis(180);

/// Dimensions, en pixels à 100 % : place pour l'ombre autour du panneau, largeur du panneau,
/// hauteur de la barre de titre et d'une ligne.
const MARGIN: i32 = 28;
const WIDTH: i32 = 560;
const HEADER: i32 = 52;
const ROW: i32 = 60;
/// Interrupteurs : couleur d'accent de macOS (bleu).
const ACCENT: (u8, u8, u8) = (10, 132, 255);
const TEXT_OFF: COLORREF = rgb(120, 120, 128);

pub(super) static SETTINGS: AtomicIsize = AtomicIsize::new(0);

#[derive(Clone, Copy, PartialEq)]
enum Row {
    Feature(Feature),
    Autostart,
}

const SECTIONS: [(&str, &[Row]); 2] = [
    (
        "Fonctionnalités",
        &[
            Row::Feature(Feature::Scenes),
            Row::Feature(Feature::Dock),
            Row::Feature(Feature::Notch),
            Row::Feature(Feature::Pin),
            Row::Feature(Feature::Launcher),
            Row::Feature(Feature::Corners),
        ],
    ),
    ("Général", &[Row::Autostart]),
];

/// Emplacements, en coordonnées de la fenêtre.
struct Layout {
    panel: RECT,
    header: RECT,
    /// Fermer, réduire, agrandir (ce dernier toujours grisé).
    lights: [RECT; 3],
    sections: Vec<(&'static str, RECT, RECT)>,
    rows: Vec<(Row, RECT)>,
}

pub(super) struct SettingsUi {
    pub hwnd: HWND,
    open: bool,
    /// Au premier plan : les boutons de la barre de titre sont en couleur.
    active: bool,
    dpi: i32,
    autostart: bool,
    /// La souris survole les boutons de la barre de titre.
    lights_hover: bool,
    /// Ligne sous le bouton de la souris, enfoncé.
    pressed: Option<Row>,
    /// Interrupteurs en train de basculer, et depuis quand.
    anims: Vec<(Row, Instant)>,
    font_title: HFONT,
    font_row: HFONT,
    font_desc: HFONT,
    font_section: HFONT,
}

impl SettingsUi {
    pub fn new(hwnd: HWND) -> Self {
        Self {
            hwnd,
            open: false,
            active: false,
            dpi: 96,
            autostart: false,
            lights_hover: false,
            pressed: None,
            anims: Vec::new(),
            font_title: HFONT::default(),
            font_row: HFONT::default(),
            font_desc: HFONT::default(),
            font_section: HFONT::default(),
        }
    }

    fn s(&self, v: i32) -> i32 {
        v * self.dpi / 96
    }

    fn layout(&self) -> Layout {
        let s = |v: i32| self.s(v);
        let (left, right) = (s(MARGIN), s(MARGIN) + s(WIDTH));
        let header = RECT { left, top: s(MARGIN), right, bottom: s(MARGIN) + s(HEADER) };
        let light = |i: i32| {
            let x = left + s(20) + i * s(20);
            let y = header.top + s(20);
            RECT { left: x, top: y, right: x + s(12), bottom: y + s(12) }
        };
        let mut y = header.bottom;
        let mut sections = Vec::new();
        let mut rows = Vec::new();
        for (title, list) in SECTIONS {
            let label = RECT { left: left + s(28), top: y, right: right - s(28), bottom: y + s(30) };
            let top = label.bottom;
            y = top;
            for &row in list {
                rows.push((row, RECT { left: left + s(16), top: y, right: right - s(16), bottom: y + s(ROW) }));
                y += s(ROW);
            }
            sections.push((title, label, RECT { left: left + s(16), top, right: right - s(16), bottom: y }));
            y += s(10);
        }
        let panel = RECT { left, top: header.top, right, bottom: y + s(10) };
        Layout { panel, header, lights: [light(0), light(1), light(2)], sections, rows }
    }

    /// Taille de la fenêtre : le panneau et son ombre.
    fn window_size(&self) -> SIZE {
        let p = self.layout().panel;
        SIZE { cx: p.right + self.s(MARGIN), cy: p.bottom + self.s(MARGIN) }
    }

    fn switch_rect(&self, row: RECT) -> RECT {
        let (w, h) = (self.s(38), self.s(22));
        let top = (row.top + row.bottom - h) / 2;
        RECT { left: row.right - self.s(16) - w, top, right: row.right - self.s(16), bottom: top + h }
    }

    fn row_at(&self, pt: POINT) -> Option<Row> {
        self.layout().rows.into_iter().find(|&(_, r)| contains(r, pt)).map(|(row, _)| row)
    }

    fn make_fonts(&mut self) {
        unsafe {
            for font in [self.font_title, self.font_row, self.font_desc, self.font_section] {
                if !font.is_invalid() {
                    let _ = DeleteObject(font);
                }
            }
        }
        self.font_title = unsafe { make_font("Inter SemiBold", 13, 600, self.dpi) };
        self.font_row = unsafe { make_font("Inter", 13, 400, self.dpi) };
        self.font_desc = unsafe { make_font("Inter", 11, 400, self.dpi) };
        self.font_section = unsafe { make_font("Inter SemiBold", 11, 600, self.dpi) };
    }

    /// Position de la poignée d'un interrupteur, de 0 (éteint) à 1 (allumé).
    fn knob(&self, row: Row, on: bool, now: Instant) -> f32 {
        let p = self.anims.iter().find(|(r, _)| *r == row).map_or(1.0, |&(_, start)| anim::ease_out(anim::progress(start, SWITCH_ANIM, now)) as f32);
        if on {
            p
        } else {
            1.0 - p
        }
    }
}

impl App {
    pub(super) fn handle_settings_message(&mut self, msg: u32, wp: WPARAM) -> bool {
        match msg {
            WM_SHOW_SETTINGS => self.open_settings(),
            WM_TIMER if wp.0 == TIMER_SETTINGS => {
                let now = Instant::now();
                self.settings.anims.retain(|&(_, start)| anim::progress(start, SWITCH_ANIM, now) < 1.0);
                if self.settings.anims.is_empty() {
                    unsafe {
                        let _ = KillTimer(self.hwnd, TIMER_SETTINGS);
                    }
                }
                self.render_settings();
            }
            _ => return false,
        }
        true
    }

    /// Ouvre les réglages au milieu de l'écran, ou les ramène au premier plan.
    pub(super) fn open_settings(&mut self) {
        let st = &mut self.settings;
        st.autostart = autostart::is_enabled();
        if !st.open {
            let mon = self.monitor;
            unsafe {
                let handle = MonitorFromPoint(POINT { x: mon.left, y: mon.top }, MONITOR_DEFAULTTOPRIMARY);
                let (mut dpi, mut dpi_y) = (96u32, 96u32);
                let _ = GetDpiForMonitor(handle, MDT_EFFECTIVE_DPI, &mut dpi, &mut dpi_y);
                st.dpi = dpi as i32;
            }
            st.make_fonts();
            st.open = true;
            st.pressed = None;
            st.anims.clear();
            let size = st.window_size();
            let x = (mon.left + mon.right - size.cx) / 2;
            let y = mon.top + ((mon.bottom - mon.top - size.cy) / 3).max(0);
            unsafe {
                let _ = SetWindowPos(st.hwnd, None, x, y, size.cx, size.cy, SWP_NOZORDER | SWP_NOACTIVATE);
            }
            self.render_settings();
        }
        let hwnd = self.settings.hwnd;
        unsafe {
            if IsIconic(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_RESTORE);
            } else {
                let _ = ShowWindow(hwnd, SW_SHOW);
            }
        }
        win::force_foreground(hwnd);
        // Son WM_ACTIVATE est arrivé pendant qu'on était occupés ici : il n'a pas été vu.
        self.settings.active = unsafe { GetForegroundWindow() } == hwnd;
        self.render_settings();
    }

    pub(super) fn close_settings(&mut self) {
        let st = &mut self.settings;
        if !st.open {
            return;
        }
        st.open = false;
        st.anims.clear();
        unsafe {
            let _ = KillTimer(self.hwnd, TIMER_SETTINGS);
            let _ = ShowWindow(st.hwnd, SW_HIDE);
        }
    }

    /// L'option de démarrage a pu changer depuis le menu de l'icône.
    pub(super) fn refresh_settings(&mut self) {
        self.settings.autostart = autostart::is_enabled();
        self.render_settings();
    }

    fn row_state(&self, row: Row) -> (bool, bool) {
        match row {
            Row::Feature(f) => (self.features.get(f), f.parent().is_none_or(|p| self.features.get(p))),
            Row::Autostart => (self.settings.autostart, true),
        }
    }

    fn toggle_row(&mut self, row: Row) {
        let (on, available) = self.row_state(row);
        if !available {
            return;
        }
        match row {
            Row::Feature(f) => self.set_feature(f, !on),
            Row::Autostart => {
                autostart::set(!on);
                self.settings.autostart = autostart::is_enabled();
            }
        }
        self.settings.anims.retain(|(r, _)| *r != row);
        self.settings.anims.push((row, Instant::now()));
        unsafe {
            SetTimer(self.hwnd, TIMER_SETTINGS, 15, None);
        }
        self.render_settings();
    }

    /// Messages de la fenêtre des réglages ; `None` pour le traitement par défaut.
    pub(super) fn settings_input(&mut self, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
        let st = &mut self.settings;
        match msg {
            WM_NCHITTEST => {
                // La barre de titre déplace la fenêtre ; l'ombre ne réagit pas.
                let mut pt = POINT { x: (lp.0 & 0xFFFF) as i16 as i32, y: ((lp.0 >> 16) & 0xFFFF) as i16 as i32 };
                unsafe {
                    let _ = ScreenToClient(st.hwnd, &mut pt);
                }
                let l = st.layout();
                let lights = RECT { right: l.lights[2].right, ..l.lights[0] };
                let hit = if !contains(l.panel, pt) {
                    HTNOWHERE
                } else if contains(l.header, pt) && !contains(inflate(lights, st.s(4)), pt) {
                    HTCAPTION
                } else {
                    HTCLIENT
                };
                return Some(LRESULT(hit as isize));
            }
            WM_MOUSEMOVE => {
                let pt = point(lp);
                let l = st.layout();
                let lights = inflate(RECT { right: l.lights[2].right, ..l.lights[0] }, st.s(4));
                let hover = contains(lights, pt);
                if hover != st.lights_hover {
                    st.lights_hover = hover;
                    self.render_settings();
                }
                let mut tme = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: self.settings.hwnd, dwHoverTime: 0 };
                unsafe {
                    let _ = TrackMouseEvent(&mut tme);
                }
            }
            WM_MOUSELEAVE => {
                st.pressed = None;
                if st.lights_hover {
                    st.lights_hover = false;
                    self.render_settings();
                }
            }
            WM_LBUTTONDOWN => st.pressed = st.row_at(point(lp)),
            WM_LBUTTONUP => {
                let pt = point(lp);
                let l = st.layout();
                if contains(inflate(l.lights[0], st.s(3)), pt) {
                    self.close_settings();
                } else if contains(inflate(l.lights[1], st.s(3)), pt) {
                    unsafe {
                        let _ = ShowWindow(st.hwnd, SW_MINIMIZE);
                    }
                } else if let Some(row) = st.row_at(pt).filter(|r| st.pressed == Some(*r)) {
                    st.pressed = None;
                    self.toggle_row(row);
                }
                self.settings.pressed = None;
            }
            WM_KEYDOWN if wp.0 as u16 == VK_ESCAPE.0 => self.close_settings(),
            WM_KEYDOWN if wp.0 as u16 == b'W' as u16 && unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0 => self.close_settings(),
            WM_ACTIVATE => {
                st.active = (wp.0 & 0xFFFF) as u32 != WA_INACTIVE;
                self.render_settings();
            }
            WM_CLOSE => self.close_settings(),
            WM_DPICHANGED => {
                st.dpi = ((wp.0 >> 16) & 0xFFFF) as i32;
                st.make_fonts();
                let suggested = unsafe { &*(lp.0 as *const RECT) };
                let size = st.window_size();
                unsafe {
                    let _ = SetWindowPos(st.hwnd, None, suggested.left, suggested.top, size.cx, size.cy, SWP_NOZORDER | SWP_NOACTIVATE);
                }
                self.render_settings();
            }
            _ => return None,
        }
        Some(LRESULT(0))
    }

    pub(super) fn render_settings(&self) {
        let st = &self.settings;
        if !st.open {
            return;
        }
        let s = |v: i32| st.s(v);
        let k = st.dpi as f32 / 96.0;
        let size = st.window_size();
        let l = st.layout();
        let white = (255, 255, 255);
        let now = Instant::now();
        let mut canvas = Canvas::new(size.cx, size.cy);
        // Ombre de fenêtre, puis le panneau gris foncé (fenêtre de macOS en mode sombre).
        canvas.round_rect(offset(inflate(l.panel, s(6)), 0, s(12)), s(22), (0, 0, 0), 30);
        canvas.round_rect(offset(inflate(l.panel, s(1)), 0, s(3)), s(13), (0, 0, 0), 70);
        let look = Material { tint: (30, 30, 32), opacity: 1.0, border: 0.16 };
        canvas.material(l.panel, s(12) as f32, None, (0, 0), &look);
        // Boutons de la barre de titre : fermer, réduire ; agrandir n'a pas de sens ici.
        let colors = [(255, 95, 87), (254, 188, 46), (75, 75, 80)];
        for (&r, color) in l.lights.iter().zip(colors) {
            let color = if st.active || st.lights_hover { color } else { (75, 75, 80) };
            canvas.round_rect(r, (r.right - r.left) / 2, color, 255);
        }
        if st.lights_hover {
            // × et − sombres dans les boutons, au survol.
            let dark = (90, 20, 15);
            let c = l.lights[0];
            let (cx, cy, a, t) = ((c.left + c.right) as f32 / 2.0, (c.top + c.bottom) as f32 / 2.0, 3.0 * k, 0.75 * k);
            canvas.fill_convex(&[(cx - a - t, cy - a + t), (cx - a + t, cy - a - t), (cx + a + t, cy + a - t), (cx + a - t, cy + a + t)], dark, 220);
            canvas.fill_convex(&[(cx + a - t, cy - a - t), (cx + a + t, cy - a + t), (cx - a + t, cy + a + t), (cx - a - t, cy + a - t)], dark, 220);
            let m = l.lights[1];
            let (mid, cy, half) = ((m.left + m.right) / 2, (m.top + m.bottom) / 2, s(4));
            let thick = ((1.5 * k).round() as i32).max(1);
            let top = cy - thick / 2;
            canvas.round_rect(RECT { left: mid - half, top, right: mid + half, bottom: top + thick }, 0, (100, 60, 0), 220);
        }
        // Groupes de lignes, séparées par un filet.
        for (_, _, group) in &l.sections {
            canvas.round_rect(*group, s(10), white, 12);
        }
        for pair in l.rows.windows(2) {
            let (a, b) = (pair[0].1, pair[1].1);
            if a.bottom == b.top {
                canvas.round_rect(RECT { left: a.left + s(16), top: a.bottom, right: a.right - s(16), bottom: a.bottom + 1 }, 0, white, 22);
            }
        }
        // Interrupteurs.
        for &(row, r) in &l.rows {
            let (on, available) = self.row_state(row);
            let t = st.knob(row, on, now);
            let alpha = |a: u8| if available { a } else { a / 3 };
            let sw = st.switch_rect(r);
            let radius = (sw.bottom - sw.top) / 2;
            canvas.round_rect(sw, radius, white, alpha(38));
            if t > 0.0 {
                canvas.round_rect(sw, radius, ACCENT, alpha((255.0 * t) as u8));
            }
            let d = sw.bottom - sw.top - s(4);
            let x = sw.left + s(2) + ((sw.right - sw.left - s(4) - d) as f32 * t).round() as i32;
            let knob = RECT { left: x, top: sw.top + s(2), right: x + d, bottom: sw.top + s(2) + d };
            canvas.round_rect(offset(knob, 0, s(1)), d / 2, (0, 0, 0), alpha(60));
            canvas.round_rect(knob, d / 2, white, if available { 255 } else { 110 });
        }
        canvas.gdi(|dc| unsafe {
            SetBkMode(dc, TRANSPARENT);
            let line = DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX;
            SelectObject(dc, st.font_title);
            draw_text(dc, "Réglages", l.header, DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX, if st.active { TEXT } else { TEXT_DIM }, 0);
            SelectObject(dc, st.font_section);
            for (title, label, _) in &l.sections {
                draw_text(dc, title, *label, line, TEXT_DIM, 0);
            }
            for &(row, r) in &l.rows {
                let (_, available) = self.row_state(row);
                let (label, description) = match row {
                    Row::Feature(f) if !available => (f.label(), "Activez l'encoche pour vous en servir"),
                    Row::Feature(f) => (f.label(), f.description()),
                    Row::Autostart => ("Lancer au démarrage de Windows", "Le Stage Manager s'ouvre avec votre session"),
                };
                let right = st.switch_rect(r).left - s(16);
                let title = RECT { left: r.left + s(16), top: r.top + s(10), right, bottom: r.top + s(31) };
                let desc = RECT { top: title.bottom, bottom: title.bottom + s(18), ..title };
                SelectObject(dc, st.font_row);
                draw_text(dc, label, title, line, if available { TEXT } else { TEXT_OFF }, 0);
                SelectObject(dc, st.font_desc);
                draw_text(dc, description, desc, line, if available { TEXT_DIM } else { TEXT_OFF }, 0);
            }
        });
        let mut rc = RECT::default();
        unsafe {
            let _ = GetWindowRect(st.hwnd, &mut rc);
        }
        canvas.present(st.hwnd, POINT { x: rc.left, y: rc.top });
    }
}
