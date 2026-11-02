//! Animations : des miniatures DWM se déplacent sur une fenêtre transparente
//! pendant que les vraies fenêtres sont réduites ou restaurées en coulisse.

use std::{
    ffi::c_void,
    mem::size_of,
    time::{Duration, Instant},
};

use windows::{
    core::*,
    Win32::{
        Foundation::*,
        Graphics::{Dwm::*, Gdi::*},
        UI::WindowsAndMessaging::*,
    },
};

use crate::stages::Wid;

/// Une miniature qui se déplace de `from` à `to` (coordonnées de l'overlay).
pub struct Flight {
    pub thumb: isize,
    pub from: RECT,
    pub to: RECT,
    /// Les fenêtres entrantes apparaissent en fondu.
    pub fade_in: bool,
}

/// Changement de scène animé.
pub struct Switch {
    pub start: Instant,
    pub duration: Duration,
    pub flights: Vec<Flight>,
    /// Fenêtres à restaurer à l'arrivée, de l'arrière vers l'avant, puis celle à activer.
    pub restore: Vec<Wid>,
    pub focus: Option<Wid>,
    /// Fenêtres dont on a coupé les animations de Windows le temps du changement.
    pub quiet: Vec<Wid>,
    /// Moment où les vraies fenêtres ont été restaurées.
    pub landed: Option<Instant>,
}

pub fn progress(start: Instant, duration: Duration, now: Instant) -> f64 {
    if duration.is_zero() {
        return 1.0;
    }
    (now.saturating_duration_since(start).as_secs_f64() / duration.as_secs_f64()).min(1.0)
}

/// Démarre vite et freine en douceur.
pub fn ease_out(t: f64) -> f64 {
    1.0 - (1.0 - t).powi(3)
}

/// Ressort amorti : dépasse légèrement la cible (≈ 1,5 %) puis s'y pose, comme sur macOS.
pub fn spring(t: f64) -> f64 {
    if t >= 1.0 {
        return 1.0;
    }
    // Ressort souple : le mouvement occupe toute la durée au lieu d'arriver dès le premier tiers.
    const ZETA: f64 = 0.8;
    const OMEGA: f64 = 6.5;
    let raw = |t: f64| {
        let wd = OMEGA * (1.0 - ZETA * ZETA).sqrt();
        let decay = (-ZETA * OMEGA * t).exp();
        1.0 - decay * ((wd * t).cos() + ZETA * OMEGA / wd * (wd * t).sin())
    };
    // Le ressort n'est jamais tout à fait posé : on rattrape l'écart restant en fin de course
    // pour arriver exactement à 1 sans à-coup.
    raw(t) + (1.0 - raw(1.0)) * t.powi(4)
}

/// Fondu enchaîné en fin de trajet (0 → 1 sur le dernier tiers) : la miniature plate qui vole
/// laisse la place à la carte inclinée de la barre.
pub fn cross_fade(t: f64) -> f64 {
    let x = ((t - 0.65) / 0.35).clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// Opacité d'une miniature qui apparaît : fondu sur le premier tiers de l'animation.
pub fn fade_in(t: f64) -> u8 {
    (255.0 * ease_out((t / 0.35).min(1.0))) as u8
}

pub fn lerp(a: RECT, b: RECT, t: f64) -> RECT {
    let l = |x: i32, y: i32| x + ((y - x) as f64 * t).round() as i32;
    RECT { left: l(a.left, b.left), top: l(a.top, b.top), right: l(a.right, b.right), bottom: l(a.bottom, b.bottom) }
}

pub fn size(r: RECT) -> SIZE {
    SIZE { cx: r.right - r.left, cy: r.bottom - r.top }
}

/// Place une source de taille `src` dans `area` en gardant ses proportions (calée en bas).
pub fn fit(area: RECT, src: SIZE) -> RECT {
    let (aw, ah) = (area.right - area.left, area.bottom - area.top);
    if src.cx <= 0 || src.cy <= 0 {
        return area;
    }
    let scale = f64::min(aw as f64 / src.cx as f64, ah as f64 / src.cy as f64);
    let (w, h) = ((src.cx as f64 * scale) as i32, (src.cy as f64 * scale) as i32);
    let left = area.left + (aw - w) / 2;
    RECT { left, top: area.bottom - h, right: left + w, bottom: area.bottom }
}

pub fn offset(r: RECT, dx: i32, dy: i32) -> RECT {
    RECT { left: r.left + dx, top: r.top + dy, right: r.right + dx, bottom: r.bottom + dy }
}

pub fn inflate(r: RECT, d: i32) -> RECT {
    RECT { left: r.left - d, top: r.top - d, right: r.right + d, bottom: r.bottom + d }
}

fn area(r: RECT) -> i64 {
    (r.right - r.left).max(0) as i64 * (r.bottom - r.top).max(0) as i64
}

fn intersect(a: RECT, b: RECT) -> RECT {
    RECT { left: a.left.max(b.left), top: a.top.max(b.top), right: a.right.min(b.right), bottom: a.bottom.min(b.bottom) }
}

/// Garde les fenêtres qu'on voit vraiment : une fenêtre cachée à plus de 80 % par une
/// fenêtre placée devant elle est écartée, car deux miniatures superposées clignotent
/// (DWM ne garantit pas leur ordre d'affichage). Entrée et sortie de l'avant vers l'arrière.
pub fn visible_layers(front_first: &[(Wid, RECT)]) -> Vec<(Wid, RECT)> {
    let mut kept: Vec<(Wid, RECT)> = Vec::new();
    for &(w, r) in front_first {
        let covered = kept.iter().any(|&(_, front)| area(intersect(r, front)) * 10 >= area(r) * 8);
        if !covered {
            kept.push((w, r));
        }
    }
    kept
}

pub fn place_thumb(thumb: isize, dest: RECT, visible: bool, opacity: u8) {
    let props = DWM_THUMBNAIL_PROPERTIES {
        dwFlags: DWM_TNP_RECTDESTINATION | DWM_TNP_VISIBLE | DWM_TNP_OPACITY | DWM_TNP_SOURCECLIENTAREAONLY,
        rcDestination: dest,
        opacity,
        fVisible: visible.into(),
        fSourceClientAreaOnly: false.into(),
        ..Default::default()
    };
    unsafe {
        let _ = DwmUpdateThumbnailProperties(thumb, &props);
    }
}

/// Active ou coupe les animations de Windows (réduction, restauration) d'une fenêtre.
pub fn set_transitions(h: HWND, enabled: bool) {
    let disabled = BOOL::from(!enabled);
    unsafe {
        let _ = DwmSetWindowAttribute(
            h,
            DWMWA_TRANSITIONS_FORCEDISABLED,
            &disabled as *const BOOL as *const c_void,
            size_of::<BOOL>() as u32,
        );
    }
}

pub fn window_rect(h: HWND) -> RECT {
    let mut rect = RECT::default();
    unsafe {
        let _ = GetWindowRect(h, &mut rect);
    }
    rect
}

/// Emplacement qu'aura la fenêtre une fois restaurée, en coordonnées écran.
pub fn restored_rect(h: HWND) -> RECT {
    unsafe {
        if !IsIconic(h).as_bool() {
            return window_rect(h);
        }
        let mut wp = WINDOWPLACEMENT { length: size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
        let _ = GetWindowPlacement(h, &mut wp);
        let monitor = MonitorFromRect(&wp.rcNormalPosition, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(monitor, &mut mi);
        if wp.flags.0 & WPF_RESTORETOMAXIMIZED.0 != 0 {
            return mi.rcWork;
        }
        if GetWindowLongPtrW(h, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0 != 0 {
            return wp.rcNormalPosition;
        }
        // Coordonnées « espace de travail » : relatives à la zone de travail de l'écran.
        offset(wp.rcNormalPosition, mi.rcWork.left - mi.rcMonitor.left, mi.rcWork.top - mi.rcMonitor.top)
    }
}

/// Fixe l'endroit où une fenêtre réduite reviendra quand on la restaurera.
pub fn set_restore_position(h: HWND, rect: RECT, maximized: bool) {
    unsafe {
        let mut wp = WINDOWPLACEMENT { length: size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
        if GetWindowPlacement(h, &mut wp).is_err() {
            return;
        }
        wp.showCmd = SW_SHOWMINNOACTIVE.0 as u32;
        if maximized {
            wp.flags = WINDOWPLACEMENT_FLAGS(wp.flags.0 | WPF_RESTORETOMAXIMIZED.0);
        } else {
            wp.flags = WINDOWPLACEMENT_FLAGS(wp.flags.0 & !WPF_RESTORETOMAXIMIZED.0);
            // Coordonnées « espace de travail » : relatives à la zone de travail de l'écran.
            let monitor = MonitorFromRect(&rect, MONITOR_DEFAULTTONEAREST);
            let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
            let _ = GetMonitorInfoW(monitor, &mut mi);
            wp.rcNormalPosition = offset(rect, mi.rcMonitor.left - mi.rcWork.left, mi.rcMonitor.top - mi.rcWork.top);
        }
        let _ = SetWindowPlacement(h, &wp);
    }
}

/// Fenêtre plein écran transparente et traversable par la souris, qui porte les miniatures animées
/// (une par écran).
pub unsafe fn create_overlay(instance: HINSTANCE) -> Result<HWND> {
    let class = w!("StageManagerOverlay");
    let wc = WNDCLASSW {
        hInstance: instance,
        lpszClassName: class,
        lpfnWndProc: Some(overlay_proc),
        hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
        ..Default::default()
    };
    // Déjà inscrite par l'overlay d'un autre écran : on la réutilise.
    if RegisterClassW(&wc) == 0 && GetLastError() != ERROR_CLASS_ALREADY_EXISTS {
        return Err(Error::from_win32());
    }
    let h = CreateWindowExW(
        WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
        class,
        w!("Stage Manager (animations)"),
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
    // Le noir devient transparent : seules les miniatures sont visibles.
    SetLayeredWindowAttributes(h, COLORREF(0), 255, LWA_COLORKEY)?;
    Ok(h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn le_ressort_depasse_un_peu_puis_se_pose() {
        let samples: Vec<f64> = (0..=1000).map(|i| spring(i as f64 / 1000.0)).collect();
        assert_eq!(samples[0], 0.0);
        let peak = samples.iter().copied().fold(0.0, f64::max);
        assert!(peak > 1.005 && peak < 1.03, "dépassement : {peak}");
        // Le mouvement doit occuper la durée : pas déjà arrivé au premier tiers.
        assert!(samples[333] < 0.9, "trop rapide : {}", samples[333]);
        // Pas de saut visible quand on force la valeur finale à 1.
        assert!((samples[999] - 1.0).abs() < 0.005, "fin : {}", samples[999]);
    }

    #[test]
    fn une_fenetre_cachee_derriere_une_autre_ne_vole_pas() {
        let r = |l, t, rr, b| RECT { left: l, top: t, right: rr, bottom: b };
        let windows = [
            (1, r(0, 0, 1000, 1000)),     // devant
            (2, r(0, 0, 1000, 1000)),     // exactement derrière : écartée
            (3, r(900, 0, 1900, 1000)),   // à côté, recouverte à 10 % : gardée
            (4, r(100, 100, 900, 900)),   // entièrement derrière la 1 : écartée
        ];
        let kept: Vec<Wid> = visible_layers(&windows).iter().map(|(w, _)| *w).collect();
        assert_eq!(kept, vec![1, 3]);
    }

    #[test]
    fn le_fondu_va_de_transparent_a_opaque() {
        assert_eq!(fade_in(0.0), 0);
        assert_eq!(fade_in(0.35), 255);
        assert_eq!(fade_in(1.0), 255);
    }
}

unsafe extern "system" fn overlay_proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_NCHITTEST {
        return LRESULT(-1); // HTTRANSPARENT
    }
    DefWindowProcW(h, msg, wp, lp)
}
