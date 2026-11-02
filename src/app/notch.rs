//! L'encoche en haut de chaque écran, comme celle des MacBook Pro : noire, collée au bord.
//! Fermée, elle affiche l'heure ; pendant la lecture, elle s'élargit un peu et montre un petit
//! symbole de lecture. Au survol, elle s'agrandit et montre le lecteur complet.
//! Une page peut y être accrochée (voir `pin`) : fermée, l'encoche affiche alors son cours ;
//! ouverte, la page elle-même, en direct.

use super::{
    dockbar::{clock_text, text_width},
    pin::parse_ticker,
    *,
};
use crate::{canvas::round_corners, media::filetime_now};

const TIMER_NOTCH_HOVER: usize = 5;
const TIMER_NOTCH_EQ: usize = 6;
const MENU_PIN: usize = 30;
const MENU_UNPIN: usize = 31;

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
/// Zone de dépôt, quand on fait glisser une fenêtre vers le haut de l'écran : elle invite à la
/// lâcher, puis grandit quand la fenêtre est dessus.
const DROP: Shape = Shape { w: 300.0, h: 62.0, radius: 22.0 };
const DROP_HOT: Shape = Shape { w: 340.0, h: 84.0, radius: 26.0 };
/// Distance au haut de l'écran (à 100 %) à partir de laquelle l'encoche invite à déposer la
/// fenêtre, et marge autour de la zone de dépôt.
const DROP_APPROACH: f32 = 220.0;
const DROP_MARGIN: f32 = 24.0;
/// Bleu de macOS, pour la zone de dépôt.
const ACCENT: (u8, u8, u8) = (10, 132, 255);
/// Aperçu de la page accrochée : sa largeur, ses hauteurs extrêmes, sa marge, et son haut
/// quand il n'y a pas de lecteur au-dessus (sous la bande de l'encoche fermée).
const CHART_W: f32 = 520.0;
const CHART_H: (f32, f32) = (200.0, 420.0);
const CHART_PAD: f32 = 16.0;
const CHART_TOP: f32 = 40.0;
/// Hausse et baisse du cours, aux couleurs d'Apple.
const UP: COLORREF = rgb(48, 209, 88);
const DOWN: COLORREF = rgb(255, 69, 58);
/// Congés creux qui raccordent l'encoche au bord de l'écran.
const SHOULDER: f32 = 6.0;
/// Place autour de l'encoche pour son ombre et le léger dépassement du ressort.
const MARGIN: f32 = 30.0;

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

/// État de la zone de dépôt : on approche une fenêtre, elle est au-dessus, ou elle vient
/// d'être lâchée (épinglée ou désépinglée). `unpin` : la fenêtre est déjà épinglée.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum DropZone {
    Near { unpin: bool },
    Hot { unpin: bool },
    Done { pinned: bool },
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
    /// Milieu du haut de son écran : l'encoche y est centrée.
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
    /// Miniature DWM de la page accrochée, montrée dans l'encoche ouverte.
    thumb: Option<isize>,
    /// Zone de dépôt pendant qu'on fait glisser une fenêtre (voir `float`).
    pub drop: Option<DropZone>,
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
            thumb: None,
            drop: None,
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
    /// Minuteurs d'une encoche (posés sur sa fenêtre) : le survol, l'égaliseur.
    pub(super) fn notch_timer(&mut self, hwnd: HWND, timer: usize) {
        let Some(s) = self.notch_index(hwnd) else { return };
        match timer {
            TIMER_NOTCH_HOVER => {
                unsafe {
                    let _ = KillTimer(hwnd, TIMER_NOTCH_HOVER);
                }
                if self.screens[s].notch.hover {
                    self.set_notch_open(s, true);
                }
            }
            // L'égaliseur danse (et la barre de progression avance) ; pendant une animation,
            // l'encoche est déjà redessinée à chaque image.
            TIMER_NOTCH_EQ => {
                if self.screens[s].notch.start.is_none() {
                    self.render_notch(s);
                }
            }
            _ => {}
        }
    }

    // --- Mise en place ----------------------------------------------------------------

    /// Une encoche en haut de chaque écran.
    pub(super) fn enable_notch(&mut self) {
        for s in 0..self.screens.len() {
            self.enable_notch_on(s);
        }
    }

    pub(super) fn enable_notch_on(&mut self, s: usize) {
        self.screens[s].notch.shown = true;
        self.place_notch(s);
        let n = &self.screens[s].notch;
        if !n.fullscreen {
            unsafe {
                let _ = ShowWindow(n.hwnd, SW_SHOWNOACTIVATE);
            }
        }
        self.retarget_notch(s);
        if self.screens[s].notch_band.is_none() {
            self.screens[s].notch_band = Some(RECT::default());
        }
        self.reserve_notch_band(s);
    }

    pub(super) fn disable_notch(&mut self) {
        for s in 0..self.screens.len() {
            let n = &mut self.screens[s].notch;
            n.shown = false;
            n.open = false;
            n.drop = None;
            n.hover = false;
            n.hover_control = None;
            n.start = None;
            n.from = REST;
            n.target = REST;
            unsafe {
                let _ = ShowWindow(n.hwnd, SW_HIDE);
                let _ = KillTimer(n.hwnd, TIMER_NOTCH_HOVER);
                let _ = KillTimer(n.hwnd, TIMER_NOTCH_EQ);
            }
            self.drop_notch_thumb(s);
            self.follow_notch(s);
            self.release_notch_band(s);
        }
    }

    /// Réserve, auprès de Windows, la bande du haut de l'écran où se trouve l'encoche fermée :
    /// rien ne passe derrière elle. La réservation passe par la barre des scènes, déjà inscrite
    /// (voir `register_appbar`) : elle couvre toute la largeur de l'écran, comme la barre des
    /// menus du Mac.
    pub(super) fn reserve_notch_band(&mut self, s: usize) {
        let sc = &mut self.screens[s];
        let Some(current) = sc.notch_band else { return };
        let height = (REST.h * sc.dpi as f32 / 96.0).round() as i32;
        let mut abd = APPBARDATA {
            cbSize: size_of::<APPBARDATA>() as u32,
            hWnd: sc.hwnd,
            uEdge: ABE_TOP,
            rc: sc.rect,
            ..Default::default()
        };
        abd.rc.bottom = abd.rc.top + height;
        unsafe {
            SHAppBarMessage(ABM_QUERYPOS, &mut abd);
            abd.rc.bottom = abd.rc.top + height;
            if abd.rc != current {
                SHAppBarMessage(ABM_SETPOS, &mut abd);
                sc.notch_band = Some(abd.rc);
            }
        }
    }

    /// Rend la bande de l'encoche aux fenêtres. La barre reste inscrite, pour le plein écran.
    fn release_notch_band(&mut self, s: usize) {
        if self.screens[s].notch_band.take().is_none() {
            return;
        }
        let mut abd = APPBARDATA { cbSize: size_of::<APPBARDATA>() as u32, hWnd: self.screens[s].hwnd, ..Default::default() };
        unsafe {
            SHAppBarMessage(ABM_REMOVE, &mut abd);
        }
        self.register_appbar(s);
    }

    /// Demi-largeur de l'encoche fermée, congés compris, en pixels : la barre des scènes se
    /// cale à côté. Au moins celle de la lecture, pour ne pas sauter à chaque pause.
    pub(super) fn notch_edge(&self, s: usize) -> Option<i32> {
        let n = &self.screens[s].notch;
        if !n.shown {
            return None;
        }
        let (closed, _) = self.notch_shapes(s);
        Some(((closed.w.max(LIVE.w) / 2.0 + SHOULDER) * n.dpi as f32 / 96.0).round() as i32)
    }

    /// L'encoche est ouverte (lecteur, page accrochée) : elle déborde sur la barre des scènes.
    pub(super) fn notch_open(&self, s: usize) -> bool {
        let n = &self.screens[s].notch;
        n.shown && !n.fullscreen && (n.open || n.drop.is_some())
    }

    /// Centre l'encoche en haut de son écran, à la bonne échelle.
    pub(super) fn place_notch(&mut self, s: usize) {
        let sc = &mut self.screens[s];
        let (mon, n) = (sc.rect, &mut sc.notch);
        n.dpi = sc.dpi;
        unsafe {
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
        // L'échelle a pu changer : la forme en cours aussi.
        self.retarget_notch(s);
    }

    /// Une appli passe en plein écran sur cet écran (ou en sort) : son encoche s'efface, et ne
    /// dessine plus rien.
    pub(super) fn notch_fullscreen(&mut self, s: usize, fullscreen: bool) {
        self.screens[s].notch.fullscreen = fullscreen;
        if fullscreen {
            self.drop_notch_thumb(s);
        }
        // Désactivée, elle reste cachée, mais retient l'état du plein écran pour plus tard.
        let n = &self.screens[s].notch;
        if !n.shown {
            return;
        }
        unsafe {
            let _ = ShowWindow(n.hwnd, if fullscreen { SW_HIDE } else { SW_SHOWNOACTIVATE });
        }
        self.retarget_notch(s);
    }

    // --- États ------------------------------------------------------------------------

    fn set_notch_open(&mut self, s: usize, open: bool) {
        if self.screens[s].notch.open != open {
            self.screens[s].notch.open = open;
            self.retarget_notch(s);
        }
    }

    /// Formes de l'encoche fermée et ouverte, d'après la musique et la page accrochée.
    fn notch_shapes(&self, s: usize) -> (Shape, Shape) {
        let n = &self.screens[s].notch;
        let media = self.now_playing.is_some();
        let playing = self.now_playing.as_ref().is_some_and(|np| np.playing);
        let k = n.dpi as f32 / 96.0;
        let mut closed = if playing { LIVE } else { REST };
        let ticker = self.ticker_width(s);
        if ticker > 0 {
            // L'heure à gauche, le cours à droite (avant le symbole de lecture). Arrondie, la
            // largeur ne suit pas chaque petit mouvement du cours.
            let (time, _) = clock_text();
            let px = |v: i32| n.s(v);
            let play = if playing { px(20) } else { 0 };
            let w = (px(16) + text_width(n.font_time, &time) + px(18) + ticker + px(16) + play) as f32 / k;
            closed.w = closed.w.max((w / 16.0).ceil() * 16.0);
        }
        let open = match self.pin_chart_size() {
            Some((w, h)) => Shape {
                w: (w + 2.0 * CHART_PAD).max(if media { OPEN.w } else { 0.0 }),
                h: if media { OPEN.h } else { CHART_TOP } + h + CHART_PAD,
                radius: OPEN.radius,
            },
            None if media => OPEN,
            None => EMPTY,
        };
        (closed, open)
    }

    /// Choisit la forme de chaque encoche d'après la musique et la souris.
    pub(super) fn notch_retarget(&mut self) {
        for s in 0..self.screens.len() {
            self.retarget_notch(s);
        }
    }

    /// Choisit la forme d'une encoche d'après la musique et la souris, et l'y amène en douceur.
    fn retarget_notch(&mut self, s: usize) {
        let playing = self.now_playing.as_ref().is_some_and(|np| np.playing);
        let (closed, open) = self.notch_shapes(s);
        let n = &mut self.screens[s].notch;
        let target = match n.drop {
            Some(DropZone::Near { .. }) => DROP,
            Some(_) => DROP_HOT,
            None if n.open => open,
            None => closed,
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
                SetTimer(n.hwnd, TIMER_NOTCH_EQ, 66, None);
            } else {
                let _ = KillTimer(n.hwnd, TIMER_NOTCH_EQ);
            }
        }
        self.render_notch(s);
        // La barre des scènes se cale contre l'encoche, et s'efface quand elle s'ouvre.
        self.follow_notch(s);
    }

    pub(super) fn notch_animating(&self) -> bool {
        self.screens.iter().any(|sc| sc.notch.start.is_some())
    }

    pub(super) fn notch_tick(&mut self, now: Instant) {
        for s in 0..self.screens.len() {
            let Some(start) = self.screens[s].notch.start else { continue };
            if anim::progress(start, ANIM, now) >= 1.0 {
                self.screens[s].notch.start = None;
            }
            self.render_notch(s);
        }
    }

    /// Arrondit la pochette (et en tire la couleur de l'égaliseur) quand le morceau change.
    fn update_notch_art(&mut self, s: usize) {
        let art = self.now_playing.as_ref().and_then(|np| np.art.clone());
        let key = art.as_ref().map_or(0, |a| Arc::as_ptr(a) as usize);
        let n = &mut self.screens[s].notch;
        if n.art.as_ref().map_or(0, |a| a.0) == key {
            return;
        }
        n.art = art.map(|img| {
            let mut rounded = (*img).clone();
            rounded.round_corners(img.w as f32 * 0.19);
            (key, Rc::new(rounded), accent(&img))
        });
    }

    // --- Dessin -----------------------------------------------------------------------

    pub(super) fn render_notches(&mut self) {
        for s in 0..self.screens.len() {
            self.render_notch(s);
        }
    }

    fn render_notch(&mut self, s: usize) {
        if !self.screens[s].notch.shown || self.screens[s].notch.fullscreen {
            return;
        }
        self.update_notch_art(s);
        let now = Instant::now();
        let n = &self.screens[s].notch;
        let px = |v: i32| n.s(v);
        let k = n.dpi as f32 / 96.0;
        let shape = n.shape(now);
        let (r, size) = n.frame(shape);
        let np = self.now_playing.as_ref();
        let (_, full) = self.notch_shapes(s);
        let pinned = self.pin_shown();
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
            canvas.round_rect(offset(inflate(r, px(6)), 0, px(10)), radius + px(6), (0, 0, 0), (35.0 * opened) as u8);
            canvas.round_rect(offset(inflate(r, px(1)), 0, px(3)), radius + px(1), (0, 0, 0), (60.0 * opened) as u8);
        }
        canvas.notch(r, shape.radius * k, SHOULDER * k, (0, 0, 0), 255);
        if let Some(drop) = n.drop {
            // On fait glisser une fenêtre : l'encoche devient une zone de dépôt.
            draw_drop(&mut canvas, r, drop, n.font_title, k, t);
            let pos = POINT { x: n.anchor.x - size.cx / 2, y: n.anchor.y };
            canvas.present(n.hwnd, pos);
            self.screens[s].notch.window = RECT { left: pos.x, top: pos.y, right: pos.x + size.cx, bottom: pos.y + size.cy };
            self.place_notch_thumb(s, None);
            return;
        }

        // Encoche fermée : l'heure au milieu. Pendant la lecture, l'encoche s'élargit, l'heure
        // glisse à gauche et un petit symbole de lecture apparaît à droite. Le cours de la page
        // accrochée prend place entre les deux.
        let closed = (1.0 - opened * 4.0).clamp(0.0, 1.0);
        if closed > 0.0 {
            let mut layer = Canvas::new(size.cx, size.cy);
            let band = px(REST.h as i32);
            let live = if playing { ((shape.w - REST.w) / (LIVE.w - REST.w)).clamp(0.0, 1.0) } else { 0.0 };
            let ticker = self.ticker_parts(s);
            let (time, _) = clock_text();
            let tw = text_width(n.font_time, &time);
            let middle = (r.left + r.right - tw) / 2;
            let slide = if ticker.is_empty() { live } else { 1.0 };
            let x = middle + ((r.left + px(16) - middle) as f32 * slide).round() as i32;
            if live > 0.0 {
                let (cx, cy) = ((r.right - px(20)) as f32, band as f32 / 2.0);
                let (w, h) = (9.0 * k, 11.0 * k);
                let left = cx - w / 2.0 + 1.0 * k;
                let triangle = [(left, cy - h / 2.0), (left + w, cy), (left, cy + h / 2.0)];
                layer.fill_convex(&round_corners(&triangle, 1.5 * k), accent, (255.0 * live) as u8);
            }
            let text = RECT { left: x, top: 0, right: x + tw + px(4), bottom: band };
            // Le cours, calé à droite : symbole, prix, variation.
            let right = r.right - px(16) - (px(20) as f32 * live).round() as i32;
            let start = right - ticker.iter().map(|p| p.3).sum::<i32>() - px(6) * (ticker.len() as i32 - 1).max(0);
            layer.gdi(|dc| unsafe {
                SetBkMode(dc, TRANSPARENT);
                SelectObject(dc, n.font_time);
                draw_text(dc, &time, text, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX, TEXT, 0);
                let mut left = start;
                for (part, font, color, width) in &ticker {
                    SelectObject(dc, *font);
                    let rect = RECT { left, top: 0, right: left + width, bottom: band };
                    draw_text(dc, part, rect, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX, *color, 0);
                    left += width + px(6);
                }
            });
            canvas.overlay_inside(&layer, closed);
        }

        // Encoche ouverte : le lecteur complet, qui apparaît en fin d'ouverture.
        let content = ((opened - 0.7) / 0.3).clamp(0.0, 1.0);
        let mut thumb = None;
        if content > 0.0 {
            let mut layer = Canvas::new(size.cx, size.cy);
            let centered = DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX;
            match np {
                // Rien à lire : la page accrochée occupe toute l'encoche.
                None if pinned => {}
                None => {
                    let text = RECT { top: r.top + px(28), bottom: r.bottom - px(6), ..r };
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
                        None => layer.round_rect(l.art, px(12), (60, 60, 68), 255),
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
            // La page accrochée, sous le lecteur. Sa miniature DWM se pose par-dessus une fois
            // l'encoche dessinée ; fermée, la page laisse place à une invitation à la rouvrir.
            if pinned {
                let top = if np.is_some() { OPEN.h } else { CHART_TOP };
                let pad = px(CHART_PAD as i32);
                let area = RECT { left: r.left + pad, top: r.top + px(top as i32), right: r.right - pad, bottom: r.bottom - pad };
                match self.pin_source() {
                    Some(source) => {
                        let dest = fit(area, anim::size(source));
                        layer.round_rect(dest, px(6), (28, 28, 32), 255);
                        thumb = Some((dest, source, (255.0 * content) as u8));
                    }
                    None => {
                        layer.round_rect(area, px(14), (34, 34, 40), 255);
                        let invite = format!("Cliquer pour ouvrir {}", self.pin.url.as_deref().map_or("la page", host));
                        layer.gdi(|dc| unsafe {
                            SetBkMode(dc, TRANSPARENT);
                            SelectObject(dc, n.font_artist);
                            draw_text(dc, &invite, area, centered | DT_END_ELLIPSIS, TEXT_DIM, 0);
                        });
                    }
                }
            }
            canvas.overlay_inside(&layer, content);
        }

        let pos = POINT { x: n.anchor.x - size.cx / 2, y: n.anchor.y };
        canvas.present(n.hwnd, pos);
        self.screens[s].notch.window = RECT { left: pos.x, top: pos.y, right: pos.x + size.cx, bottom: pos.y + size.cy };
        self.place_notch_thumb(s, thumb);
    }

    // --- Page accrochée -----------------------------------------------------------------

    /// Taille de l'aperçu de la page accrochée, en pixels à 100 %, aux proportions de sa fenêtre.
    fn pin_chart_size(&self) -> Option<(f32, f32)> {
        if !self.pin_shown() {
            return None;
        }
        let (w, h) = self.pin_source().map_or((16.0, 9.0), |r| ((r.right - r.left) as f32, (r.bottom - r.top) as f32));
        Some((CHART_W, (CHART_W * h / w).clamp(CHART_H.0, CHART_H.1)))
    }

    /// Le cours affiché dans l'encoche fermée, en morceaux (voir `title_parts`).
    fn ticker_parts(&self, s: usize) -> Vec<(String, HFONT, COLORREF, i32)> {
        if !self.pin_shown() {
            return Vec::new();
        }
        let n = &self.screens[s].notch;
        title_parts(&self.pin.title, n.font_time, n.font_artist, n.s(200))
    }

    fn ticker_width(&self, s: usize) -> i32 {
        let parts = self.ticker_parts(s);
        parts.iter().map(|p| p.3).sum::<i32>() + self.screens[s].notch.s(6) * (parts.len() as i32 - 1).max(0)
    }

    /// Pose (ou retire) la miniature de la page accrochée dans une encoche : où, quelle partie
    /// de la fenêtre, avec quelle opacité.
    fn place_notch_thumb(&mut self, s: usize, thumb: Option<(RECT, RECT, u8)>) {
        let (Some((dest, source, opacity)), Some(page)) = (thumb, self.pin.hwnd) else {
            self.drop_notch_thumb(s);
            return;
        };
        let n = &mut self.screens[s].notch;
        if n.thumb.is_none() {
            n.thumb = unsafe { DwmRegisterThumbnail(n.hwnd, page) }.ok();
        }
        let Some(thumb) = n.thumb else { return };
        let props = DWM_THUMBNAIL_PROPERTIES {
            dwFlags: DWM_TNP_RECTDESTINATION | DWM_TNP_RECTSOURCE | DWM_TNP_VISIBLE | DWM_TNP_OPACITY | DWM_TNP_SOURCECLIENTAREAONLY,
            rcDestination: dest,
            rcSource: source,
            opacity,
            fVisible: true.into(),
            fSourceClientAreaOnly: false.into(),
        };
        unsafe {
            let _ = DwmUpdateThumbnailProperties(thumb, &props);
        }
    }

    /// Retire la miniature de la page accrochée de toutes les encoches.
    pub(super) fn notch_drop_thumb(&mut self) {
        for s in 0..self.screens.len() {
            self.drop_notch_thumb(s);
        }
    }

    pub(super) fn drop_notch_thumb(&mut self, s: usize) {
        if let Some(thumb) = self.screens[s].notch.thumb.take() {
            unsafe {
                let _ = DwmUnregisterThumbnail(thumb);
            }
        }
    }

    /// Repasse les encoches devant les autres fenêtres « toujours au premier plan » (la page
    /// épinglée).
    pub(super) fn raise_notches(&self) {
        for sc in self.screens.iter().filter(|sc| sc.notch.shown && !sc.notch.fullscreen) {
            unsafe {
                let _ = SetWindowPos(sc.notch.hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            }
        }
    }

    // --- Zone de dépôt ------------------------------------------------------------------

    /// Montre (ou retire) la zone de dépôt dans une encoche, qui y prend sa forme d'un mouvement
    /// de ressort.
    pub(super) fn set_notch_drop(&mut self, s: usize, drop: Option<DropZone>) {
        let n = &mut self.screens[s].notch;
        if !n.shown {
            return;
        }
        if n.drop != drop {
            n.drop = drop;
            self.retarget_notch(s);
        } else if matches!(drop, Some(DropZone::Near { .. })) && n.start.is_none() {
            // La punaise sautille pour inviter à lâcher la fenêtre.
            self.render_notch(s);
        }
    }

    /// Encoche vers laquelle on fait glisser une fenêtre (souris en `pt`), et si la fenêtre est
    /// au-dessus de sa zone de dépôt.
    pub(super) fn notch_drop_zone(&self, pt: POINT) -> Option<(usize, bool)> {
        let s = self.screens.iter().position(|sc| contains(sc.rect, pt))?;
        let sc = &self.screens[s];
        if !sc.notch.shown || sc.notch.fullscreen {
            return None;
        }
        let k = sc.dpi as f32 / 96.0;
        let (dx, dy) = ((pt.x - sc.notch.anchor.x).abs() as f32 / k, (pt.y - sc.rect.top) as f32 / k);
        if dy > DROP_APPROACH {
            return None;
        }
        Some((s, dy <= DROP_HOT.h + DROP_MARGIN && dx <= DROP_HOT.w / 2.0 + DROP_MARGIN))
    }

    // --- Souris -----------------------------------------------------------------------

    fn notch_control_at(&self, s: usize, pt: POINT) -> Option<Control> {
        let n = &self.screens[s].notch;
        if !n.open || self.now_playing.is_none() {
            return None;
        }
        let (r, _) = n.frame(n.shape(Instant::now()));
        let l = n.layout(r);
        [(l.previous, Control::Previous), (l.button, Control::PlayPause), (l.next, Control::Next)]
            .into_iter()
            .find(|&(rect, _)| contains(rect, pt))
            .map(|(_, control)| control)
    }

    pub(super) fn notch_mouse(&mut self, hwnd: HWND, msg: u32, lp: LPARAM) {
        let Some(s) = self.notch_index(hwnd) else { return };
        let pt = point(lp);
        match msg {
            WM_MOUSEMOVE => {
                let n = &mut self.screens[s].notch;
                if !n.tracking {
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: n.hwnd,
                        dwHoverTime: 0,
                    };
                    n.tracking = unsafe { TrackMouseEvent(&mut tme) }.is_ok();
                }
                if !n.hover {
                    n.hover = true;
                    unsafe {
                        SetTimer(n.hwnd, TIMER_NOTCH_HOVER, HOVER_DELAY, None);
                    }
                }
                let control = self.notch_control_at(s, pt);
                if control != self.screens[s].notch.hover_control {
                    self.screens[s].notch.hover_control = control;
                    self.render_notch(s);
                }
            }
            WM_MOUSELEAVE => {
                let n = &mut self.screens[s].notch;
                n.tracking = false;
                n.hover = false;
                n.hover_control = None;
                unsafe {
                    let _ = KillTimer(n.hwnd, TIMER_NOTCH_HOVER);
                }
                self.set_notch_open(s, false);
            }
            WM_LBUTTONUP => match self.notch_control_at(s, pt) {
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
                // Ailleurs, un clic montre la page accrochée sur cet écran (ou la regare) :
                // l'encoche se referme pour ne pas la masquer.
                None if self.pin_shown() => {
                    self.set_notch_open(s, false);
                    let screen = self.screens[s].id;
                    self.pin_toggle(screen);
                }
                // Un clic sur l'encoche fermée l'ouvre sans attendre.
                None => self.set_notch_open(s, true),
            },
            _ => {}
        }
    }
}

/// Clic droit sur l'encoche : accrocher la page de la fenêtre active, ou la décrocher ; et les
/// réglages.
pub(super) unsafe fn notch_menu(hwnd: HWND) {
    // Le menu prend le premier plan : on retient la fenêtre qu'on regardait.
    let foreground = GetForegroundWindow();
    let Some((available, pinned)) = with_app(|app| (app.features.active(Feature::Pin), app.pin_shown())) else { return };
    let mut items = Vec::new();
    if pinned {
        items.push((MENU_UNPIN, w!("Décrocher la page")));
    } else if available {
        items.push((MENU_PIN, w!("Accrocher la page active\tCtrl+Alt+P")));
    }
    if available {
        items.push((0, PCWSTR::null()));
    }
    items.push((MENU_SETTINGS, w!("Réglages…")));
    match popup(hwnd, &items, &[]) {
        MENU_PIN => with_app(|app| app.pin_window(foreground)),
        MENU_UNPIN => with_app(App::unpin),
        MENU_SETTINGS => with_app(App::open_settings),
        _ => None,
    };
}

/// Titre de la page accrochée en morceaux (texte, police, couleur, largeur) : le cours chez
/// TradingView, qui l'écrit dans son titre (symbole, prix, variation en vert ou en rouge) ;
/// pour une autre page, le titre tout court, limité à `max` pixels.
pub(super) fn title_parts(title: &str, bold: HFONT, regular: HFONT, max: i32) -> Vec<(String, HFONT, COLORREF, i32)> {
    let title = title.trim();
    if title.is_empty() {
        return Vec::new();
    }
    let part = |text: &str, font: HFONT, color: COLORREF| (text.to_owned(), font, color, text_width(font, text));
    match parse_ticker(title) {
        Some(t) => {
            let mut parts = vec![part(&t.symbol, bold, TEXT), part(&t.price, regular, TEXT)];
            if let Some((change, up)) = &t.change {
                parts.push(part(change, regular, if *up { UP } else { DOWN }));
            }
            parts
        }
        None => {
            let mut whole = part(title, regular, TEXT_DIM);
            whole.3 = whole.3.min(max);
            vec![whole]
        }
    }
}

/// Zone de dépôt : une punaise et une invitation à lâcher la fenêtre, sur fond bleu quand elle
/// est au-dessus.
fn draw_drop(canvas: &mut Canvas, r: RECT, drop: DropZone, font: HFONT, k: f32, t: f32) {
    let (text, hot) = match drop {
        DropZone::Near { unpin: false } => ("Déposer ici pour épingler", false),
        DropZone::Near { unpin: true } => ("Déposer ici pour désépingler", false),
        DropZone::Hot { unpin: false } => ("Lâcher pour épingler", true),
        DropZone::Hot { unpin: true } => ("Lâcher pour désépingler", true),
        DropZone::Done { pinned: true } => ("Épinglée au premier plan", true),
        DropZone::Done { pinned: false } => ("Désépinglée", true),
    };
    let px = |v: f32| (v * k).round() as i32;
    if hot {
        let glow = RECT { left: r.left + px(6.0), top: r.top + px(6.0), right: r.right - px(6.0), bottom: r.bottom - px(6.0) };
        canvas.round_rect(glow, px(20.0), ACCENT, 90);
    }
    let (icon, gap) = (px(16.0), px(10.0));
    let left = (r.left + r.right - icon - gap - text_width(font, text)) / 2;
    // Un peu sous le milieu : le haut de l'encoche reste la bande sombre de l'appareil photo.
    let cy = (r.top + r.bottom) as f32 / 2.0 + 3.0 * k;
    let bob = if hot { 0.0 } else { -3.0 * k * (t * 6.0).sin().abs() };
    let color = if hot { (255, 255, 255) } else { (100, 170, 255) };
    draw_pin_icon(canvas, (left as f32 + icon as f32 / 2.0, cy + bob), k, color);
    let text_rect = RECT { left: left + icon + gap, top: (cy - 10.0 * k) as i32, right: r.right, bottom: (cy + 10.0 * k) as i32 };
    canvas.gdi(|dc| unsafe {
        SetBkMode(dc, TRANSPARENT);
        SelectObject(dc, font);
        draw_text(dc, text, text_rect, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX, if hot { TEXT } else { TEXT_DIM }, 0);
    });
}

/// Punaise, penchée : tête, corps qui s'évase, collerette, pointe.
fn draw_pin_icon(canvas: &mut Canvas, (cx, cy): (f32, f32), k: f32, color: (u8, u8, u8)) {
    let (sin, cos) = 0.6f32.sin_cos();
    let p = |x: f32, y: f32| (cx + (x * cos - y * sin) * k, cy + (x * sin + y * cos) * k);
    let parts: [&[(f32, f32)]; 4] = [
        &[(-3.5, -9.0), (3.5, -9.0), (3.5, -6.5), (-3.5, -6.5)],
        &[(-2.5, -6.5), (2.5, -6.5), (3.5, 0.5), (-3.5, 0.5)],
        &[(-6.0, 0.5), (6.0, 0.5), (6.0, 2.5), (-6.0, 2.5)],
        &[(-0.9, 2.5), (0.9, 2.5), (0.0, 9.5)],
    ];
    for part in parts {
        let points: Vec<(f32, f32)> = part.iter().map(|&(x, y)| p(x, y)).collect();
        canvas.fill_convex(&points, color, 255);
    }
}

/// Nom du site d'une adresse : « tradingview.com ».
pub(super) fn host(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    host.strip_prefix("www.").unwrap_or(host)
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

    #[test]
    fn nom_du_site() {
        assert_eq!(host("https://www.tradingview.com/chart/abc/?symbol=BTC"), "tradingview.com");
        assert_eq!(host("http://example.org"), "example.org");
    }
}
