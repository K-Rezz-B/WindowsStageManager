//! Fenêtres épinglées. On fait glisser une fenêtre vers le haut de l'écran : l'encoche devient
//! une zone de dépôt ; lâchée dessus, la fenêtre reste au premier plan, à sa place d'avant.
//! Réduite, elle laisse place à une vignette en direct dans le coin en haut à droite de son
//! écran (la fenêtre, elle, est garée hors de l'écran, où elle continue de se dessiner). Un clic
//! sur la vignette la rouvre ; redéposée sur l'encoche, elle est désépinglée. Une seule fenêtre
//! épinglée à la fois.
//!
//! La barre d'ancrage de Windows qui descend au même endroit est coupée (voir `snapbar`) ;
//! ce réglage ne prend effet qu'à la prochaine ouverture de session.

use super::{
    dockbar::text_width,
    notch::{host, title_parts, DropZone},
    pin::{is_parked, monitor_work, window_source, TITLE_BAR},
    *,
};
use crate::snapbar;

const TIMER_FLOAT: usize = 10;
const TIMER_FLOAT_DRAG: usize = 11;
const TIMER_FLOAT_DROP: usize = 12;
const TIMER_FLOAT_DONE: usize = 13;
/// Surveillance de la fenêtre épinglée (fermeture, titre), et plus serrée quand elle change d'état.
const POLL: u32 = 500;
const POLL_FAST: u32 = 50;
/// Suivi de la souris pendant qu'on fait glisser une fenêtre : Windows ne prévient pas.
const DRAG_POLL: u32 = 30;
/// Distance minimale parcourue par la souris (à 100 %) pour que ce soit un vrai glisser.
const MIN_MOVE: i32 = 40;
/// Après le lâcher, le temps que Windows finisse de placer la fenêtre (il l'agrandit quand la
/// souris a touché le haut de l'écran).
const DROP_DELAY: u32 = 80;
/// Durée du « Épinglée » affiché dans l'encoche après le lâcher.
const DONE_DELAY: u32 = 1100;
/// Distance entre le bord droit des écrans et la fenêtre garée (plus loin que la page accrochée).
const PARK_GAP: i32 = 400;
/// Vignette, en pixels à 100 % : taille maximale de l'aperçu (aux proportions de la fenêtre),
/// marge autour de lui, bandeau du titre, écart avec les bords de l'écran, rayon des coins,
/// place pour l'ombre (sur les côtés, en bas).
const VIGNETTE: (f32, f32) = (400.0, 300.0);
const VIGNETTE_PAD: i32 = 6;
const VIGNETTE_HEADER: i32 = 30;
const VIGNETTE_GAP: i32 = 12;
const VIGNETTE_RADIUS: i32 = 14;
const VIGNETTE_SHADOW: (i32, i32) = (8, 16);

/// Fenêtre qu'on a mise « toujours au premier plan » : à lui retirer quoi qu'il arrive en quittant.
static TOPMOST: AtomicIsize = AtomicIsize::new(0);
/// Fenêtre réduite en vignette, garée hors de l'écran, et sa place à l'écran.
static PARKED: Mutex<Option<(isize, RECT)>> = Mutex::new(None);

pub(super) struct Float {
    /// La fenêtre épinglée.
    pub hwnd: Option<HWND>,
    /// Elle est réduite en vignette.
    pub reduced: bool,
    /// Son titre, dans le bandeau de la vignette.
    title: String,
    /// Fenêtre qu'on est en train de faire glisser.
    drag: Option<FloatDrag>,
    /// Fenêtre qu'on vient de lâcher sur l'encoche de cet écran : on l'épingle dans un instant.
    dropped: Option<(FloatDrag, usize)>,
    vignette: Vignette,
}

impl Float {
    pub fn new() -> Self {
        Self { hwnd: None, reduced: false, title: String::new(), drag: None, dropped: None, vignette: Vignette::default() }
    }
}

/// Fenêtre qu'on fait glisser, avec sa place d'avant.
#[derive(Clone, Copy)]
struct FloatDrag {
    window: HWND,
    rect: RECT,
    maximized: bool,
    /// Elle est déjà épinglée : la déposer la désépingle.
    unpin: bool,
    /// Position de la souris au début du glisser.
    start: POINT,
}

/// Vignette de la fenêtre réduite : une fenêtre à nous, toujours au premier plan, qui la montre
/// en direct (miniature DWM) sous un bandeau avec son titre et un bouton de fermeture.
#[derive(Default)]
struct Vignette {
    /// Créée à sa première apparition.
    hwnd: HWND,
    shown: bool,
    /// Miniature DWM, et la fenêtre qu'elle montre.
    thumb: Option<(isize, HWND)>,
    /// Écran dont elle occupe le coin en haut à droite.
    screen: Option<ScreenId>,
    dpi: i32,
    /// Polices du bandeau (gras, normal), à l'échelle de son écran.
    fonts: (HFONT, HFONT),
    /// Le panneau et son bouton de fermeture, en coordonnées de la fenêtre.
    panel: RECT,
    close: RECT,
    hover_close: bool,
    tracking: bool,
}

impl App {
    pub(super) fn handle_float_message(&mut self, msg: u32, wp: WPARAM) -> bool {
        match msg {
            WM_TIMER if wp.0 == TIMER_FLOAT => self.float_poll(),
            WM_TIMER if wp.0 == TIMER_FLOAT_DRAG => self.float_drag_tick(),
            WM_TIMER if wp.0 == TIMER_FLOAT_DROP => {
                unsafe {
                    let _ = KillTimer(self.hwnd, TIMER_FLOAT_DROP);
                }
                if let Some((drag, s)) = self.float.dropped.take() {
                    self.float_drop(drag, s);
                }
            }
            WM_TIMER if wp.0 == TIMER_FLOAT_DONE => {
                unsafe {
                    let _ = KillTimer(self.hwnd, TIMER_FLOAT_DONE);
                }
                for s in 0..self.screens.len() {
                    if matches!(self.screens[s].notch.drop, Some(DropZone::Done { .. })) {
                        self.set_notch_drop(s, None);
                    }
                }
            }
            _ => return false,
        }
        true
    }

    // --- Mise en place ----------------------------------------------------------------

    pub(super) fn enable_float(&mut self) {
        snapbar::disable();
    }

    /// `restore_snap` : la fonctionnalité est désactivée, la barre d'ancrage de Windows revient.
    /// (En quittant, on la laisse coupée : sinon l'Explorateur redémarrerait à chaque fois.)
    pub(super) fn disable_float(&mut self, restore_snap: bool) {
        unsafe {
            for timer in [TIMER_FLOAT_DRAG, TIMER_FLOAT_DROP, TIMER_FLOAT_DONE] {
                let _ = KillTimer(self.hwnd, timer);
            }
        }
        self.float.drag = None;
        self.float.dropped = None;
        self.clear_notch_drops();
        self.unpin_float();
        if restore_snap {
            snapbar::restore();
        }
    }

    // --- Événements -------------------------------------------------------------------

    /// Événement Windows : une fenêtre qu'on fait glisser, ou la fenêtre épinglée. Renvoie
    /// `true` s'il est traité (il ne doit toucher ni à la page accrochée ni aux scènes).
    pub(super) fn float_event(&mut self, event: u32, h: HWND) -> bool {
        if !self.features.active(Feature::Float) {
            return false;
        }
        match event {
            EVENT_SYSTEM_MOVESIZESTART => {
                self.float_drag_start(h);
                return false;
            }
            EVENT_SYSTEM_MOVESIZEEND => {
                if self.float.drag.is_some_and(|d| d.window == h) {
                    self.float_drag_end();
                }
                return false;
            }
            _ => {}
        }
        if self.float.hwnd != Some(h) {
            return false;
        }
        match event {
            EVENT_SYSTEM_MINIMIZESTART => self.reduce_float(),
            // Choisie avec Alt+Tab : elle quitte sa vignette.
            EVENT_SYSTEM_FOREGROUND if self.float.reduced => self.expand_float(false),
            // Elle vient de passer devant tout : les encoches et les coins repassent devant elle.
            EVENT_SYSTEM_FOREGROUND => self.raise_over_float(),
            EVENT_OBJECT_HIDE | EVENT_OBJECT_DESTROY => {
                self.float_poll_soon();
                return false;
            }
            _ => return false,
        }
        true
    }

    // --- Glisser vers l'encoche -----------------------------------------------------------

    fn float_drag_start(&mut self, h: HWND) {
        let movable = win::is_app_window(h) || self.float.hwnd == Some(h) || self.is_pin_window(h);
        let mut pt = POINT::default();
        if !movable || unsafe { GetCursorPos(&mut pt) }.is_err() {
            return;
        }
        let maximized = unsafe { IsZoomed(h) }.as_bool();
        let unpin = self.float.hwnd == Some(h);
        self.float.drag = Some(FloatDrag { window: h, rect: anim::window_rect(h), maximized, unpin, start: pt });
        unsafe {
            SetTimer(self.hwnd, TIMER_FLOAT_DRAG, DRAG_POLL, None);
        }
    }

    /// Encoche visée par la fenêtre qu'on fait glisser, et si elle est sur sa zone de dépôt.
    fn float_target(&self, drag: FloatDrag) -> Option<(usize, bool)> {
        // Un redimensionnement par le haut passe aussi par là : on ne retient que les
        // déplacements (une fenêtre agrandie reprend sa taille normale dès qu'on la tire).
        if !drag.maximized && size(anim::window_rect(drag.window)) != size(drag.rect) {
            return None;
        }
        let mut pt = POINT::default();
        unsafe { GetCursorPos(&mut pt) }.ok()?;
        // Un simple clic sur la barre de titre (d'une fenêtre agrandie, sous l'encoche…) passe
        // aussi par là : il faut que la souris ait vraiment bougé.
        let dpi = self.screens.iter().find(|sc| contains(sc.rect, pt)).map_or(96, |sc| sc.dpi);
        let moved = ((pt.x - drag.start.x) as f32).hypot((pt.y - drag.start.y) as f32);
        if moved < (MIN_MOVE * dpi / 96) as f32 {
            return None;
        }
        self.notch_drop_zone(pt)
    }

    /// La fenêtre se déplace : l'encoche vers laquelle elle va l'invite à la déposer.
    fn float_drag_tick(&mut self) {
        let drag = self.float.drag.filter(|d| unsafe { IsWindow(d.window) }.as_bool());
        let Some(drag) = drag else {
            self.float.drag = None;
            unsafe {
                let _ = KillTimer(self.hwnd, TIMER_FLOAT_DRAG);
            }
            self.clear_notch_drops();
            return;
        };
        let target = self.float_target(drag);
        for s in 0..self.screens.len() {
            let drop = match target {
                Some((t, true)) if t == s => Some(DropZone::Hot { unpin: drag.unpin }),
                Some((t, false)) if t == s => Some(DropZone::Near { unpin: drag.unpin }),
                _ => None,
            };
            self.set_notch_drop(s, drop);
        }
    }

    /// Fenêtre lâchée : sur la zone de dépôt, elle est épinglée (ou désépinglée) dans un instant.
    fn float_drag_end(&mut self) {
        unsafe {
            let _ = KillTimer(self.hwnd, TIMER_FLOAT_DRAG);
        }
        let Some(drag) = self.float.drag.take() else { return };
        let Some((s, true)) = self.float_target(drag) else {
            self.clear_notch_drops();
            return;
        };
        for o in 0..self.screens.len() {
            let drop = (o == s).then_some(DropZone::Done { pinned: !drag.unpin });
            self.set_notch_drop(o, drop);
        }
        self.float.dropped = Some((drag, s));
        unsafe {
            SetTimer(self.hwnd, TIMER_FLOAT_DROP, DROP_DELAY, None);
            SetTimer(self.hwnd, TIMER_FLOAT_DONE, DONE_DELAY, None);
        }
    }

    fn clear_notch_drops(&mut self) {
        for s in 0..self.screens.len() {
            self.set_notch_drop(s, None);
        }
    }

    /// La fenêtre lâchée sur l'encoche de l'écran `s` retourne à sa place d'avant le glisser,
    /// épinglée ou désépinglée. Agrandie avant, elle garde sa taille normale, au milieu de l'écran.
    fn float_drop(&mut self, drag: FloatDrag, s: usize) {
        let h = drag.window;
        if !unsafe { IsWindow(h) }.as_bool() {
            return;
        }
        unsafe {
            // Windows l'a agrandie si la souris a touché le haut de l'écran.
            if IsZoomed(h).as_bool() {
                let _ = ShowWindow(h, SW_RESTORE);
            }
        }
        let place = if drag.maximized {
            let r = anim::window_rect(h);
            let (w, hh) = (r.right - r.left, r.bottom - r.top);
            let work = self.screens.get(s).map_or(r, |sc| monitor_work(sc.handle));
            let (x, y) = (((work.left + work.right - w) / 2).max(work.left), ((work.top + work.bottom - hh) / 2).max(work.top));
            RECT { left: x, top: y, right: x + w, bottom: y + hh }
        } else {
            drag.rect
        };
        if drag.unpin {
            self.unpin_float();
            unsafe {
                let _ = SetWindowPos(h, None, place.left, place.top, place.right - place.left, place.bottom - place.top, SWP_NOZORDER | SWP_NOACTIVATE);
            }
        } else {
            self.pin_float(h, place);
        }
    }

    // --- Épingler, réduire -------------------------------------------------------------

    /// Épingle la fenêtre au premier plan, en `place` (une seule à la fois).
    fn pin_float(&mut self, h: HWND, place: RECT) {
        if self.float.hwnd.is_some_and(|old| old != h) {
            self.unpin_float();
        }
        self.float.hwnd = Some(h);
        self.float.reduced = false;
        self.float.title = win::title(h);
        if self.pin.hwnd == Some(h) {
            // La page accrochée : elle ne retourne plus au garage.
            self.pin.peek = true;
        }
        TOPMOST.store(h.0 as isize, Ordering::Relaxed);
        unsafe {
            let _ = SetWindowPos(h, HWND_TOPMOST, place.left, place.top, place.right - place.left, place.bottom - place.top, SWP_NOACTIVATE);
        }
        // Pas d'animation de Windows quand elle part en vignette.
        anim::set_transitions(h, false);
        self.raise_over_float();
        win::force_foreground(h);
        unsafe {
            SetTimer(self.hwnd, TIMER_FLOAT, POLL, None);
        }
    }

    /// Désépingle la fenêtre : elle reprend sa place (si elle était en vignette) et son rang
    /// parmi les autres fenêtres.
    pub(super) fn unpin_float(&mut self) {
        let Some(h) = self.float.hwnd.take() else { return };
        self.float.reduced = false;
        self.hide_vignette();
        restore_parked();
        restore_topmost();
        if self.pin.hwnd != Some(h) {
            anim::set_transitions(h, true);
        }
        unsafe {
            let _ = KillTimer(self.hwnd, TIMER_FLOAT);
        }
    }

    /// Réduite (bouton de réduction, Win + Bas, clic sur l'encoche pour la page) : elle part en
    /// vignette dans le coin de son écran. Une fenêtre réduite ne se dessinerait plus : on la
    /// rouvre aussitôt, garée hors de l'écran.
    fn reduce_float(&mut self) {
        let Some(h) = self.float.hwnd else { return };
        if !self.float.reduced {
            self.float.reduced = true;
            // Pour une fenêtre réduite, l'écran où elle reviendra.
            self.float.vignette.screen = self.screen_at(h);
        }
        if self.pin.hwnd == Some(h) {
            self.pin.peek = false;
        }
        if win::is_minimized(h) {
            win::show(h, SW_SHOWNOACTIVATE);
            self.float_poll_soon();
        } else {
            self.park_float(h);
        }
    }

    /// La vignette redevient la fenêtre, à sa place et au premier plan.
    fn expand_float(&mut self, focus: bool) {
        let Some(h) = self.float.hwnd else { return };
        self.float.reduced = false;
        restore_parked();
        if self.pin.hwnd == Some(h) {
            self.pin.peek = true;
        }
        self.hide_vignette();
        self.raise_over_float();
        if focus {
            win::force_foreground(h);
        }
        unsafe {
            SetTimer(self.hwnd, TIMER_FLOAT, POLL, None);
        }
    }

    /// Clic sur l'encoche quand la page accrochée est épinglée : de l'écran à sa vignette, et
    /// inversement.
    pub(super) fn toggle_float_reduced(&mut self) {
        if self.float.reduced {
            self.expand_float(true);
        } else {
            self.reduce_float();
        }
    }

    /// Pousse la fenêtre hors de l'écran, où elle continue de se dessiner, et montre sa vignette.
    fn park_float(&mut self, h: HWND) {
        if !is_parked(h) {
            let home = anim::window_rect(h);
            *PARKED.lock().unwrap_or_else(|e| e.into_inner()) = Some((h.0 as isize, home));
            unsafe {
                let x = GetSystemMetrics(SM_XVIRTUALSCREEN) + GetSystemMetrics(SM_CXVIRTUALSCREEN) + PARK_GAP;
                let _ = SetWindowPos(h, None, x, home.top, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
                // Le clavier ne doit pas rester sur une fenêtre invisible.
                if GetForegroundWindow() == h {
                    if let Some(next) = self.window_below(h) {
                        win::force_foreground(next);
                    }
                }
            }
        }
        self.render_vignette();
    }

    fn float_poll_soon(&mut self) {
        unsafe {
            SetTimer(self.hwnd, TIMER_FLOAT, POLL_FAST, None);
        }
    }

    /// Surveille la fenêtre épinglée : fermée ou cachée, elle est oubliée ; réduite en vignette,
    /// elle est garée dès que Windows l'a rouverte, et son titre suit dans le bandeau.
    fn float_poll(&mut self) {
        let Some(h) = self.float.hwnd else {
            unsafe {
                let _ = KillTimer(self.hwnd, TIMER_FLOAT);
            }
            return;
        };
        if !win::is_alive(h) {
            self.float.hwnd = None;
            self.float.reduced = false;
            TOPMOST.store(0, Ordering::Relaxed);
            PARKED.lock().unwrap_or_else(|e| e.into_inner()).take();
            self.hide_vignette();
            unsafe {
                let _ = KillTimer(self.hwnd, TIMER_FLOAT);
            }
            return;
        }
        let mut delay = POLL;
        if self.float.reduced {
            if win::is_minimized(h) {
                win::show(h, SW_SHOWNOACTIVATE);
                delay = POLL_FAST;
            } else if !is_parked(h) {
                self.park_float(h);
            }
            let title = win::title(h);
            if title != self.float.title {
                self.float.title = title;
                self.render_vignette();
            }
        } else if win::is_minimized(h) {
            // Réduite sans qu'on l'ait vu (Win + M…).
            self.reduce_float();
            delay = POLL_FAST;
        }
        unsafe {
            SetTimer(self.hwnd, TIMER_FLOAT, delay, None);
        }
    }

    /// Les encoches, les coins de l'écran et la vignette repassent devant la fenêtre épinglée.
    fn raise_over_float(&self) {
        self.raise_notches();
        self.corners.raise();
        if self.float.vignette.shown {
            unsafe {
                let _ = SetWindowPos(self.float.vignette.hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            }
        }
    }

    // --- Vignette ------------------------------------------------------------------------

    /// Dessine la vignette dans le coin en haut à droite de son écran si la fenêtre épinglée
    /// est réduite, la cache sinon (et devant une appli plein écran, comme l'encoche).
    pub(super) fn render_vignette(&mut self) {
        let wanted = self.features.active(Feature::Float) && self.float.reduced;
        let s = self.float.vignette.screen.and_then(|id| self.screen_index(id)).or((!self.screens.is_empty()).then_some(0));
        let (true, Some(s), Some(window)) = (wanted, s, self.float.hwnd) else {
            self.hide_vignette();
            return;
        };
        // La page accrochée : sans sa barre de titre, comme dans l'encoche.
        let page = self.pin.hwnd == Some(window);
        let source = window_source(window, if page { TITLE_BAR } else { 0 });
        let Some(source) = source.filter(|_| !self.screens[s].fullscreen) else {
            self.hide_vignette();
            return;
        };
        let Some(hwnd) = self.vignette_window() else { return };
        let (monitor, dpi) = (self.screens[s].handle, self.screens[s].dpi);
        let px = |v: i32| v * dpi / 96;
        let k = dpi as f32 / 96.0;
        let v = &mut self.float.vignette;
        if v.dpi != dpi || v.fonts.0.is_invalid() {
            unsafe {
                for font in [v.fonts.0, v.fonts.1] {
                    if !font.is_invalid() {
                        let _ = DeleteObject(font);
                    }
                }
                v.fonts = (make_font("Inter SemiBold", 12, 600, dpi), make_font("Inter", 12, 400, dpi));
            }
            v.dpi = dpi;
        }

        // L'aperçu, aux proportions de la fenêtre, dans un panneau noir avec le bandeau au-dessus.
        let (sw, sh) = ((source.right - source.left) as f32, (source.bottom - source.top) as f32);
        let scale = (VIGNETTE.0 / sw).min(VIGNETTE.1 / sh) * k;
        let (tw, th) = ((sw * scale).round() as i32, (sh * scale).round() as i32);
        let (pad, header, side) = (px(VIGNETTE_PAD), px(VIGNETTE_HEADER), px(VIGNETTE_SHADOW.0));
        let panel = RECT { left: side, top: 0, right: side + tw + 2 * pad, bottom: header + th + pad };
        let size = SIZE { cx: panel.right + side, cy: panel.bottom + px(VIGNETTE_SHADOW.1) };
        let dest = RECT { left: panel.left + pad, top: panel.top + header, right: panel.right - pad, bottom: panel.bottom - pad };
        let r = px(9);
        let (cx, cy) = (panel.right - pad - px(3) - r, panel.top + header / 2);
        let close = RECT { left: cx - r, top: cy - r, right: cx + r, bottom: cy + r };
        v.panel = panel;
        v.close = close;
        let work = monitor_work(monitor);
        let gap = px(VIGNETTE_GAP);
        let pos = POINT { x: work.right - gap - panel.right, y: work.top + gap };

        let radius = px(VIGNETTE_RADIUS);
        let mut canvas = Canvas::new(size.cx, size.cy);
        canvas.round_rect(offset(inflate(panel, px(6)), 0, px(8)), radius + px(6), (0, 0, 0), 35);
        canvas.round_rect(offset(inflate(panel, px(1)), 0, px(2)), radius + px(1), (0, 0, 0), 60);
        canvas.round_rect(panel, radius, (0, 0, 0), 255);
        canvas.round_rect(dest, px(6), (28, 28, 32), 255);
        if v.hover_close {
            canvas.round_rect(close, r, (255, 255, 255), 45);
        }
        draw_cross(&mut canvas, close, k, if v.hover_close { 255 } else { 170 });
        // Le bandeau : le cours chez TradingView (ou le site) pour la page accrochée, le titre
        // de la fenêtre sinon.
        let (bold, regular) = v.fonts;
        let limit = close.left - px(8);
        let max = limit - dest.left;
        let parts = if page {
            let parts = title_parts(&self.pin.title, bold, regular, max);
            if parts.is_empty() {
                title_parts(self.pin.url.as_deref().map_or("Page accrochée", host), bold, regular, max)
            } else {
                parts
            }
        } else {
            let title = self.float.title.trim();
            vec![(title.to_owned(), regular, TEXT, text_width(regular, title).min(max))]
        };
        canvas.gdi(|dc| unsafe {
            SetBkMode(dc, TRANSPARENT);
            let mut left = dest.left + px(4);
            for (part, font, color, width) in &parts {
                if left >= limit {
                    break;
                }
                SelectObject(dc, *font);
                let rect = RECT { left, top: panel.top, right: (left + width).min(limit), bottom: panel.top + header };
                draw_text(dc, part, rect, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX, *color, 0);
                left += width + px(6);
            }
        });
        canvas.present(hwnd, pos);
        if !v.shown {
            // Devant les autres fenêtres « toujours au premier plan ».
            unsafe {
                let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                let _ = SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            }
            v.shown = true;
        }

        // La fenêtre en direct, par-dessus.
        if v.thumb.is_some_and(|(_, from)| from != window) {
            if let Some((thumb, _)) = v.thumb.take() {
                unsafe {
                    let _ = DwmUnregisterThumbnail(thumb);
                }
            }
        }
        if v.thumb.is_none() {
            v.thumb = unsafe { DwmRegisterThumbnail(hwnd, window) }.ok().map(|thumb| (thumb, window));
        }
        let Some((thumb, _)) = v.thumb else { return };
        let props = DWM_THUMBNAIL_PROPERTIES {
            dwFlags: DWM_TNP_RECTDESTINATION | DWM_TNP_RECTSOURCE | DWM_TNP_VISIBLE | DWM_TNP_OPACITY | DWM_TNP_SOURCECLIENTAREAONLY,
            rcDestination: dest,
            rcSource: source,
            opacity: 255,
            fVisible: true.into(),
            fSourceClientAreaOnly: false.into(),
        };
        unsafe {
            let _ = DwmUpdateThumbnailProperties(thumb, &props);
        }
    }

    fn hide_vignette(&mut self) {
        let v = &mut self.float.vignette;
        if let Some((thumb, _)) = v.thumb.take() {
            unsafe {
                let _ = DwmUnregisterThumbnail(thumb);
            }
        }
        if v.shown {
            unsafe {
                let _ = ShowWindow(v.hwnd, SW_HIDE);
            }
            v.shown = false;
        }
        v.hover_close = false;
    }

    /// Fenêtre de la vignette, créée à sa première apparition : toujours au premier plan, elle
    /// ne prend jamais le clavier.
    fn vignette_window(&mut self) -> Option<HWND> {
        if !self.float.vignette.hwnd.is_invalid() {
            return Some(self.float.vignette.hwnd);
        }
        unsafe {
            let instance: HINSTANCE = GetModuleHandleW(None).ok()?.into();
            let class = w!("StageManagerVignette");
            let wc = WNDCLASSW {
                hInstance: instance,
                lpszClassName: class,
                lpfnWndProc: Some(vignette_proc),
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                ..Default::default()
            };
            RegisterClassW(&wc);
            let hwnd = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
                class,
                w!("Fenêtre épinglée"),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                instance,
                None,
            )
            .ok()?;
            self.float.vignette.hwnd = hwnd;
            Some(hwnd)
        }
    }

    /// Souris sur la vignette : sa croix désépingle la fenêtre (qui reprend sa place), un clic
    /// ailleurs la rouvre en grand.
    fn vignette_mouse(&mut self, msg: u32, lp: LPARAM) {
        let pt = point(lp);
        let v = &mut self.float.vignette;
        match msg {
            WM_MOUSEMOVE => {
                if !v.tracking {
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: v.hwnd,
                        dwHoverTime: 0,
                    };
                    v.tracking = unsafe { TrackMouseEvent(&mut tme) }.is_ok();
                }
                let hover = contains(v.close, pt);
                if hover != v.hover_close {
                    v.hover_close = hover;
                    self.render_vignette();
                }
            }
            WM_MOUSELEAVE => {
                v.tracking = false;
                if v.hover_close {
                    v.hover_close = false;
                    self.render_vignette();
                }
            }
            WM_LBUTTONUP if contains(v.close, pt) => self.unpin_float(),
            WM_LBUTTONUP if contains(v.panel, pt) => self.expand_float(true),
            _ => {}
        }
    }
}

/// Fenêtre de la vignette.
unsafe extern "system" fn vignette_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_ERASEBKGND => LRESULT(1),
        WM_MOUSEMOVE | WM_MOUSELEAVE | WM_LBUTTONUP => {
            with_app(|app| app.vignette_mouse(msg, lp));
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// Croix du bouton de fermeture, centrée dans `r` : deux branches épaissies.
fn draw_cross(canvas: &mut Canvas, r: RECT, k: f32, alpha: u8) {
    let (cx, cy) = ((r.left + r.right) as f32 / 2.0, (r.top + r.bottom) as f32 / 2.0);
    let (arm, half) = (3.5 * k, 0.8 * k);
    // Écart perpendiculaire à chaque branche, pour son épaisseur.
    let n = half / std::f32::consts::SQRT_2;
    for dir in [1.0, -1.0] {
        let (dx, dy) = (arm, dir * arm);
        let (nx, ny) = (-dir * n, n);
        let branch = [(cx - dx + nx, cy - dy + ny), (cx + dx + nx, cy + dy + ny), (cx + dx - nx, cy + dy - ny), (cx - dx - nx, cy - dy - ny)];
        canvas.fill_convex(&branch, (255, 255, 255), alpha);
    }
}

/// Rend sa place à la fenêtre réduite en vignette.
fn restore_parked() {
    let Some((h, home)) = PARKED.lock().unwrap_or_else(|e| e.into_inner()).take() else { return };
    let h = HWND(h as _);
    unsafe {
        if IsWindow(h).as_bool() {
            let _ = SetWindowPos(h, None, home.left, home.top, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
        }
    }
}

/// Retire le « toujours au premier plan » qu'on avait donné à la fenêtre épinglée.
fn restore_topmost() {
    let h = TOPMOST.swap(0, Ordering::Relaxed);
    if h != 0 {
        unsafe {
            let _ = SetWindowPos(HWND(h as _), HWND_NOTOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        }
    }
}

/// Remet la fenêtre épinglée comme on l'a trouvée (en quittant, en cas de plantage).
pub(super) fn restore() {
    restore_parked();
    restore_topmost();
}
