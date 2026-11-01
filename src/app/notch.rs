//! L'encoche en haut de l'écran, comme celle des MacBook Pro : noire, collée au bord.
//! Fermée, elle affiche l'heure ; pendant la lecture, elle s'élargit un peu et montre un petit
//! symbole de lecture. Au survol, elle s'agrandit et montre le lecteur complet.

use super::{
    dockbar::{clock_text, text_width},
    *,
};
use crate::{canvas::round_corners, media::filetime_now};

const TIMER_NOTCH_HOVER: usize = 5;
const TIMER_NOTCH_EQ: usize = 6;

/// Temps de survol avant l'ouverture : passer en coup de vent en haut de l'écran (vers les
/// onglets du navigateur…) ne l'ouvre pas.
const HOVER_DELAY: u32 = 120;
const ANIM: Duration = Duration::from_millis(450);

/// Formes de l'encoche, en pixels à 100 % : au repos, pendant la lecture, ouverte sur le
/// lecteur, ouverte sans rien à lire.
const REST: Shape = Shape { w: 190.0, h: 32.0, radius: 10.0 };
const LIVE: Shape = Shape { w: 220.0, h: 32.0, radius: 10.0 };
const OPEN: Shape = Shape { w: 440.0, h: 186.0, radius: 30.0 };
const EMPTY: Shape = Shape { w: 320.0, h: 84.0, radius: 24.0 };
/// Congés creux qui raccordent l'encoche au bord de l'écran.
const SHOULDER: f32 = 6.0;
/// Place autour de l'encoche pour son ombre et le léger dépassement du ressort.
const MARGIN: f32 = 30.0;

pub(super) static NOTCH: AtomicIsize = AtomicIsize::new(0);

#[derive(Clone, Copy, PartialEq)]
struct Shape {
    w: f32,
    h: f32,
    radius: f32,
}

impl Shape {
    fn lerp(self, to: Shape, t: f32) -> Shape {
        let l = |a: f32, b: f32| a + (b - a) * t;
        Shape { w: l(self.w, to.w), h: l(self.h, to.h), radius: l(self.radius, to.radius) }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Control {
    Previous,
    PlayPause,
    Next,
}

/// Emplacement des éléments du lecteur dans l'encoche ouverte.
struct OpenLayout {
    art: RECT,
    title: RECT,
    artist: RECT,
    equalizer: RECT,
    elapsed: RECT,
    bar: RECT,
    remaining: RECT,
    previous: RECT,
    button: RECT,
    next: RECT,
}

/// Pochette aux coins arrondis et sa couleur dominante, calculées une fois par morceau
/// (repérée par l'adresse de l'image d'origine).
type Cover = (usize, Rc<Image>, (u8, u8, u8));

pub(super) struct NotchUi {
    pub hwnd: HWND,
    shown: bool,
    /// Masquée devant une appli en plein écran (jeu, vidéo).
    fullscreen: bool,
    dpi: i32,
    /// Milieu du haut de l'écran principal : l'encoche y est centrée.
    anchor: POINT,
    /// Position de la fenêtre à l'écran (elle suit la taille de l'encoche).
    window: RECT,
    tracking: bool,
    hover: bool,
    open: bool,
    hover_control: Option<Control>,
    from: Shape,
    target: Shape,
    start: Option<Instant>,
    /// Horloge de l'égaliseur.
    epoch: Instant,
    art: Option<Cover>,
    font_title: HFONT,
    font_artist: HFONT,
    font_small: HFONT,
    font_time: HFONT,
}

impl NotchUi {
    pub fn new(hwnd: HWND) -> Self {
        Self {
            hwnd,
            shown: false,
            fullscreen: false,
            dpi: 96,
            anchor: POINT::default(),
            window: RECT::default(),
            tracking: false,
            hover: false,
            open: false,
            hover_control: None,
            from: REST,
            target: REST,
            start: None,
            epoch: Instant::now(),
            art: None,
            font_title: HFONT::default(),
            font_artist: HFONT::default(),
            font_small: HFONT::default(),
            font_time: HFONT::default(),
        }
    }

    fn s(&self, v: i32) -> i32 {
        v * self.dpi / 96
    }

    /// Forme actuelle, en cours d'animation ou non.
    fn shape(&self, now: Instant) -> Shape {
        match self.start {
            Some(start) => self.from.lerp(self.target, anim::spring(anim::progress(start, ANIM, now)) as f32),
            None => self.target,
        }
    }

    /// Rectangle de l'encoche dans sa fenêtre, et taille de la fenêtre.
    fn frame(&self, shape: Shape) -> (RECT, SIZE) {
        let k = self.dpi as f32 / 96.0;
        let pad = ((SHOULDER + MARGIN) * k).round() as i32;
        let (w, h) = ((shape.w * k).round() as i32, (shape.h * k).round() as i32);
        (RECT { left: pad, top: 0, right: pad + w, bottom: h }, SIZE { cx: w + 2 * pad, cy: h + pad })
    }

    fn layout(&self, r: RECT) -> OpenLayout {
        let s = |v: i32| self.s(v);
        let art = RECT { left: r.left + s(24), top: r.top + s(30), right: r.left + s(88), bottom: r.top + s(94) };
        let equalizer = RECT { left: r.right - s(46), top: art.top + s(23), right: r.right - s(24), bottom: art.top + s(41) };
        let title = RECT { left: art.right + s(14), top: art.top + s(11), right: equalizer.left - s(12), bottom: art.top + s(31) };
        let artist = RECT { top: title.bottom + s(1), bottom: title.bottom + s(21), ..title };
        // Progression : temps écoulé, barre, temps restant.
        let cy = art.bottom + s(24);
        let elapsed = RECT { left: art.left, top: cy - s(9), right: art.left + s(40), bottom: cy + s(9) };
        let remaining = RECT { left: r.right - s(64), right: r.right - s(24), ..elapsed };
        let bar = RECT { left: elapsed.right + s(8), top: cy - s(2), right: remaining.left - s(8), bottom: cy + s(3) };
        // Précédent, lecture/pause, suivant, centrés en bas.
        let (mid, cy, size) = ((r.left + r.right) / 2, cy + s(36), s(40));
        let button = RECT { left: mid - size / 2, top: cy - size / 2, right: mid + size / 2, bottom: cy + size / 2 };
        OpenLayout {
            art,
            title,
            artist,
            equalizer,
            elapsed,
            bar,
            remaining,
            previous: offset(button, -s(64), 0),
            button,
            next: offset(button, s(64), 0),
        }
    }
}

impl App {
    pub(super) fn handle_notch_message(&mut self, msg: u32, wp: WPARAM) -> bool {
        match msg {
            WM_TIMER if wp.0 == TIMER_NOTCH_HOVER => {
                unsafe {
                    let _ = KillTimer(self.hwnd, TIMER_NOTCH_HOVER);
                }
                if self.notch.hover {
                    self.set_notch_open(true);
                }
            }
            // L'égaliseur danse (et la barre de progression avance) ; pendant une animation,
            // l'encoche est déjà redessinée à chaque image.
            WM_TIMER if wp.0 == TIMER_NOTCH_EQ => {
                if !self.notch_animating() {
                    self.render_notch();
                }
            }
            _ => return false,
        }
        true
    }

    // --- Mise en place ----------------------------------------------------------------

    pub(super) fn enable_notch(&mut self) {
        self.notch.shown = true;
        self.place_notch();
        unsafe {
            let _ = ShowWindow(self.notch.hwnd, SW_SHOWNOACTIVATE);
        }
        self.notch_retarget();
    }

    pub(super) fn disable_notch(&mut self) {
        let n = &mut self.notch;
        n.shown = false;
        n.open = false;
        n.hover = false;
        n.hover_control = None;
        n.start = None;
        n.from = REST;
        n.target = REST;
        unsafe {
            let _ = ShowWindow(n.hwnd, SW_HIDE);
            let _ = KillTimer(self.hwnd, TIMER_NOTCH_HOVER);
            let _ = KillTimer(self.hwnd, TIMER_NOTCH_EQ);
        }
    }

    /// Centre l'encoche en haut de l'écran principal, à la bonne échelle.
    pub(super) fn place_notch(&mut self) {
        let mon = self.monitor;
        let n = &mut self.notch;
        unsafe {
            let handle = MonitorFromPoint(POINT::default(), MONITOR_DEFAULTTOPRIMARY);
            let (mut dpi, mut dpi_y) = (96u32, 96u32);
            let _ = GetDpiForMonitor(handle, MDT_EFFECTIVE_DPI, &mut dpi, &mut dpi_y);
            n.dpi = dpi as i32;
            for font in [n.font_title, n.font_artist, n.font_small, n.font_time] {
                if !font.is_invalid() {
                    let _ = DeleteObject(font);
                }
            }
            n.font_title = make_font("Inter SemiBold", 14, 600, n.dpi);
            n.font_artist = make_font("Inter", 13, 400, n.dpi);
            n.font_small = make_font("Inter", 10, 400, n.dpi);
            n.font_time = make_font("Inter SemiBold", 13, 600, n.dpi);
        }
        n.anchor = POINT { x: (mon.left + mon.right) / 2, y: mon.top };
        self.render_notch();
    }

    /// Une appli passe en plein écran (ou en sort) : l'encoche s'efface, et ne dessine plus rien.
    pub(super) fn notch_fullscreen(&mut self, fullscreen: bool) {
        self.notch.fullscreen = fullscreen;
        unsafe {
            let _ = ShowWindow(self.notch.hwnd, if fullscreen { SW_HIDE } else { SW_SHOWNOACTIVATE });
        }
        self.notch_retarget();
    }

    // --- États ------------------------------------------------------------------------

    fn set_notch_open(&mut self, open: bool) {
        if self.notch.open != open {
            self.notch.open = open;
            self.notch_retarget();
        }
    }

    /// Choisit la forme de l'encoche d'après la musique et la souris, et l'y amène en douceur.
    pub(super) fn notch_retarget(&mut self) {
        let media = self.now_playing.is_some();
        let playing = self.now_playing.as_ref().is_some_and(|np| np.playing);
        let n = &mut self.notch;
        let target = match (n.open, media, playing) {
            (true, true, _) => OPEN,
            (true, false, _) => EMPTY,
            (false, _, true) => LIVE,
            _ => REST,
        };
        if target != n.target {
            let now = Instant::now();
            n.from = n.shape(now);
            n.target = target;
            n.start = Some(now);
        }
        // Fermée, l'encoche ne bouge pas : l'égaliseur ne danse que dans le lecteur ouvert.
        unsafe {
            if playing && n.open && n.shown && !n.fullscreen {
                SetTimer(self.hwnd, TIMER_NOTCH_EQ, 66, None);
            } else {
                let _ = KillTimer(self.hwnd, TIMER_NOTCH_EQ);
            }
        }
        self.render_notch();
    }

    pub(super) fn notch_animating(&self) -> bool {
        self.notch.start.is_some()
    }

    pub(super) fn notch_tick(&mut self, now: Instant) {
        let Some(start) = self.notch.start else { return };
        if anim::progress(start, ANIM, now) >= 1.0 {
            self.notch.start = None;
        }
        self.render_notch();
    }

    /// Arrondit la pochette (et en tire la couleur de l'égaliseur) quand le morceau change.
    fn update_notch_art(&mut self) {
        let art = self.now_playing.as_ref().and_then(|np| np.art.clone());
        let key = art.as_ref().map_or(0, |a| Arc::as_ptr(a) as usize);
        if self.notch.art.as_ref().map_or(0, |a| a.0) == key {
            return;
        }
        self.notch.art = art.map(|img| {
            let mut rounded = (*img).clone();
            rounded.round_corners(img.w as f32 * 0.19);
            (key, Rc::new(rounded), accent(&img))
        });
    }

    // --- Dessin -----------------------------------------------------------------------

    pub(super) fn render_notch(&mut self) {
        if !self.notch.shown || self.notch.fullscreen {
            return;
        }
        self.update_notch_art();
        let now = Instant::now();
        let n = &self.notch;
        let s = |v: i32| n.s(v);
        let k = n.dpi as f32 / 96.0;
        let shape = n.shape(now);
        let (r, size) = n.frame(shape);
        let np = self.now_playing.as_ref();
        let full = if np.is_some() { OPEN } else { EMPTY };
        // 0 au repos, 1 ouverte.
        let opened = ((shape.h - REST.h) / (full.h - REST.h)).clamp(0.0, 1.0);
        let playing = np.is_some_and(|np| np.playing);
        let white = (255, 255, 255);
        let art = n.art.as_ref().map(|a| Rc::clone(&a.1));
        let accent = n.art.as_ref().map_or((235, 235, 240), |a| a.2);
        let t = now.duration_since(n.epoch).as_secs_f32();

        let mut canvas = Canvas::new(size.cx, size.cy);
        if opened > 0.0 {
            // Ombre douce sous l'encoche ouverte.
            let radius = (shape.radius * k) as i32;
            canvas.round_rect(offset(inflate(r, s(6)), 0, s(10)), radius + s(6), (0, 0, 0), (35.0 * opened) as u8);
            canvas.round_rect(offset(inflate(r, s(1)), 0, s(3)), radius + s(1), (0, 0, 0), (60.0 * opened) as u8);
        }
        canvas.notch(r, shape.radius * k, SHOULDER * k, (0, 0, 0), 255);

        // Encoche fermée : l'heure au milieu. Pendant la lecture, l'encoche s'élargit, l'heure
        // glisse à gauche et un petit symbole de lecture apparaît à droite.
        let closed = (1.0 - opened * 4.0).clamp(0.0, 1.0);
        if closed > 0.0 {
            let mut layer = Canvas::new(size.cx, size.cy);
            let band = s(REST.h as i32);
            let live = if playing { ((shape.w - REST.w) / (LIVE.w - REST.w)).clamp(0.0, 1.0) } else { 0.0 };
            let (time, _) = clock_text();
            let tw = text_width(n.font_time, &time);
            let middle = (r.left + r.right - tw) / 2;
            let x = middle + ((r.left + s(16) - middle) as f32 * live).round() as i32;
            if live > 0.0 {
                let (cx, cy) = ((r.right - s(20)) as f32, band as f32 / 2.0);
                let (w, h) = (9.0 * k, 11.0 * k);
                let left = cx - w / 2.0 + 1.0 * k;
                let triangle = [(left, cy - h / 2.0), (left + w, cy), (left, cy + h / 2.0)];
                layer.fill_convex(&round_corners(&triangle, 1.5 * k), accent, (255.0 * live) as u8);
            }
            let text = RECT { left: x, top: 0, right: x + tw + s(4), bottom: band };
            layer.gdi(|dc| unsafe {
                SetBkMode(dc, TRANSPARENT);
                SelectObject(dc, n.font_time);
                draw_text(dc, &time, text, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX, TEXT, 0);
            });
            canvas.overlay_inside(&layer, closed);
        }

        // Encoche ouverte : le lecteur complet, qui apparaît en fin d'ouverture.
        let content = ((opened - 0.7) / 0.3).clamp(0.0, 1.0);
        if content > 0.0 {
            let mut layer = Canvas::new(size.cx, size.cy);
            let centered = DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX;
            match np {
                None => {
                    let text = RECT { top: r.top + s(28), bottom: r.bottom - s(6), ..r };
                    layer.gdi(|dc| unsafe {
                        SetBkMode(dc, TRANSPARENT);
                        SelectObject(dc, n.font_artist);
                        draw_text(dc, "Aucune musique en cours", text, centered, TEXT_DIM, 0);
                    });
                }
                Some(np) => {
                    let l = n.layout(r);
                    match &art {
                        Some(img) => layer.draw_quad(img, quad(l.art), 255, 0),
                        None => layer.round_rect(l.art, s(12), (60, 60, 68), 255),
                    }
                    draw_equalizer(&mut layer, l.equalizer, t, playing, accent);
                    for (rect, control) in [(l.previous, Control::Previous), (l.button, Control::PlayPause), (l.next, Control::Next)] {
                        if n.hover_control == Some(control) {
                            layer.round_rect(rect, (rect.right - rect.left) / 2, white, 40);
                        }
                    }
                    // Icônes un peu plus grandes que dans le Dock.
                    let big = n.dpi * 4 / 3;
                    draw_play_pause(&mut layer, l.button, np.playing, big);
                    draw_skip(&mut layer, l.previous, false, if np.can_previous { 255 } else { 90 }, big);
                    draw_skip(&mut layer, l.next, true, if np.can_next { 255 } else { 90 }, big);
                    let position = np.position(filetime_now());
                    if let Some((at, duration)) = position {
                        let bar = l.bar;
                        let radius = (bar.bottom - bar.top) / 2;
                        layer.round_rect(bar, radius, white, 60);
                        let done = bar.left + ((bar.right - bar.left) as f64 * at / duration).round() as i32;
                        if done > bar.left {
                            layer.round_rect(RECT { right: done.max(bar.left + 2 * radius), ..bar }, radius, white, 230);
                        }
                    }
                    layer.gdi(|dc| unsafe {
                        SetBkMode(dc, TRANSPARENT);
                        let line = DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX;
                        SelectObject(dc, n.font_title);
                        draw_text(dc, &np.title, l.title, line, TEXT, 0);
                        SelectObject(dc, n.font_artist);
                        draw_text(dc, &np.artist, l.artist, line, TEXT_DIM, 0);
                        if let Some((at, duration)) = position {
                            SelectObject(dc, n.font_small);
                            draw_text(dc, &clock(at), l.elapsed, DT_LEFT | DT_VCENTER | DT_SINGLELINE, TEXT_DIM, 0);
                            let left = format!("-{}", clock(duration - at));
                            draw_text(dc, &left, l.remaining, DT_RIGHT | DT_VCENTER | DT_SINGLELINE, TEXT_DIM, 0);
                        }
                    });
                }
            }
            canvas.overlay_inside(&layer, content);
        }

        let pos = POINT { x: n.anchor.x - size.cx / 2, y: n.anchor.y };
        canvas.present(n.hwnd, pos);
        self.notch.window = RECT { left: pos.x, top: pos.y, right: pos.x + size.cx, bottom: pos.y + size.cy };
    }

    // --- Souris -----------------------------------------------------------------------

    fn notch_control_at(&self, pt: POINT) -> Option<Control> {
        if !self.notch.open || self.now_playing.is_none() {
            return None;
        }
        let (r, _) = self.notch.frame(self.notch.shape(Instant::now()));
        let l = self.notch.layout(r);
        [(l.previous, Control::Previous), (l.button, Control::PlayPause), (l.next, Control::Next)]
            .into_iter()
            .find(|&(rect, _)| contains(rect, pt))
            .map(|(_, control)| control)
    }

    pub(super) fn notch_mouse(&mut self, msg: u32, lp: LPARAM) {
        let pt = point(lp);
        match msg {
            WM_MOUSEMOVE => {
                if !self.notch.tracking {
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: self.notch.hwnd,
                        dwHoverTime: 0,
                    };
                    self.notch.tracking = unsafe { TrackMouseEvent(&mut tme) }.is_ok();
                }
                if !self.notch.hover {
                    self.notch.hover = true;
                    unsafe {
                        SetTimer(self.hwnd, TIMER_NOTCH_HOVER, HOVER_DELAY, None);
                    }
                }
                let control = self.notch_control_at(pt);
                if control != self.notch.hover_control {
                    self.notch.hover_control = control;
                    self.render_notch();
                }
            }
            WM_MOUSELEAVE => {
                self.notch.tracking = false;
                self.notch.hover = false;
                self.notch.hover_control = None;
                unsafe {
                    let _ = KillTimer(self.hwnd, TIMER_NOTCH_HOVER);
                }
                self.set_notch_open(false);
            }
            WM_LBUTTONUP => match self.notch_control_at(pt) {
                Some(Control::PlayPause) => {
                    self.media.toggle_play_pause();
                    // Affiché tout de suite, sans attendre la confirmation du lecteur.
                    if let Some(np) = self.now_playing.as_mut() {
                        np.playing = !np.playing;
                    }
                    self.notch_retarget();
                }
                Some(Control::Previous) => self.media.previous(),
                Some(Control::Next) => self.media.next(),
                // Un clic sur l'encoche fermée l'ouvre sans attendre.
                None => self.set_notch_open(true),
            },
            _ => {}
        }
    }
}

fn quad(r: RECT) -> [(f32, f32); 4] {
    let (l, t, rr, b) = (r.left as f32, r.top as f32, r.right as f32, r.bottom as f32);
    [(l, t), (rr, t), (rr, b), (l, b)]
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

/// Icône « précédent » ou « suivant » : deux triangles accolés.
fn draw_skip(canvas: &mut Canvas, button: RECT, forward: bool, alpha: u8, dpi: i32) {
    let k = dpi as f32 / 96.0;
    let (cx, cy) = ((button.left + button.right) as f32 / 2.0, (button.top + button.bottom) as f32 / 2.0);
    let (w, h) = (8.0 * k, 10.0 * k);
    let dir = if forward { 1.0 } else { -1.0 };
    for start in [cx - dir * w, cx] {
        let triangle = [(start, cy - h / 2.0), (start + dir * w, cy), (start, cy + h / 2.0)];
        canvas.fill_convex(&round_corners(&triangle, 1.2 * k), (255, 255, 255), alpha);
    }
}

/// Égaliseur : quatre barres qui dansent pendant la lecture, à plat en pause.
fn draw_equalizer(canvas: &mut Canvas, r: RECT, t: f32, playing: bool, color: (u8, u8, u8)) {
    const BARS: [(f32, f32); 4] = [(7.1, 0.0), (9.3, 1.7), (6.2, 3.1), (8.4, 4.6)];
    // Quatre barres et trois espaces de même largeur.
    let bw = (r.right - r.left) as f32 / 7.0;
    let cy = (r.top + r.bottom) as f32 / 2.0;
    for (i, (speed, phase)) in BARS.into_iter().enumerate() {
        let level = if playing {
            let wave = (t * speed + phase).sin() * 0.6 + (t * speed * 0.53 + phase * 2.0).sin() * 0.4;
            0.3 + 0.7 * wave.abs()
        } else {
            0.25
        };
        let h = ((r.bottom - r.top) as f32 * level).max(bw);
        let x = r.left as f32 + i as f32 * 2.0 * bw;
        let bar = RECT {
            left: x.round() as i32,
            top: (cy - h / 2.0).round() as i32,
            right: (x + bw).round() as i32,
            bottom: (cy + h / 2.0).round() as i32,
        };
        canvas.round_rect(bar, ((bw / 2.0).round() as i32).max(1), color, 255);
    }
}

/// Couleur dominante de la pochette, ravivée : celle de l'égaliseur, comme sur l'iPhone.
fn accent(img: &Image) -> (u8, u8, u8) {
    let (mut sum, mut count) = ([0f32; 3], 0f32);
    for p in img.px.iter().step_by(7).filter(|p| *p >> 24 > 128) {
        for (c, shift) in sum.iter_mut().zip([16, 8, 0]) {
            *c += ((p >> shift) & 0xFF) as f32;
        }
        count += 1.0;
    }
    let avg = sum.map(|c| c / count.max(1.0));
    let mean = (avg[0] + avg[1] + avg[2]) / 3.0;
    // Couleurs plus franches, puis éclaircies pour rester lisibles sur le noir.
    let vivid = avg.map(|c| (mean + (c - mean) * 1.6).max(0.0));
    let max = vivid[0].max(vivid[1]).max(vivid[2]);
    if count == 0.0 || max < 30.0 {
        return (235, 235, 240);
    }
    let k = 235.0 / max;
    let c = |v: f32| (v * k).min(255.0) as u8;
    (c(vivid[0]), c(vivid[1]), c(vivid[2]))
}

/// Durée au format « 3:07 ».
fn clock(seconds: f64) -> String {
    let s = seconds.max(0.0) as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durees_lisibles() {
        assert_eq!(clock(0.0), "0:00");
        assert_eq!(clock(187.6), "3:07");
        assert_eq!(clock(-2.0), "0:00");
    }

    #[test]
    fn couleur_de_la_pochette() {
        let red = Image { w: 4, h: 4, px: vec![0xFF_B4_28_28; 16] };
        let (r, g, b) = accent(&red);
        assert!(r > 200 && g < 60 && b < 60, "{r} {g} {b}");
        let black = Image { w: 4, h: 4, px: vec![0xFF_00_00_00; 16] };
        assert_eq!(accent(&black), (235, 235, 240), "une pochette noire donne du blanc");
    }
}
