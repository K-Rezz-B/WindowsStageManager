//! Instantanés des fenêtres : leur image au moment où elles quittent l'écran, pour dessiner
//! les cartes inclinées de la barre (les miniatures DWM ne savent pas se déformer).

use std::{ffi::c_void, mem::size_of};

use windows::Win32::{
    Foundation::*,
    Graphics::{Dwm::*, Gdi::*},
    Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS},
    UI::HiDpi::GetDpiForWindow,
};

use crate::{
    anim,
    canvas::{Dib, Image},
};

/// Demande à la fenêtre son rendu complet, y compris pour les applis DirectX et Chromium.
const PW_RENDERFULLCONTENT: PRINT_WINDOW_FLAGS = PRINT_WINDOW_FLAGS(2);

/// Capture une fenêtre visible, réduite à `max_w` pixels de large au plus.
/// Ne marche pas sur une fenêtre réduite : à appeler juste avant de la réduire.
pub fn capture(h: HWND, max_w: i32) -> Option<Image> {
    unsafe {
        let window = anim::window_rect(h);
        // Le rectangle de la fenêtre inclut des bordures invisibles : on garde le cadre visible.
        let mut frame = RECT::default();
        let frame_ok = DwmGetWindowAttribute(
            h,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut frame as *mut RECT as *mut c_void,
            size_of::<RECT>() as u32,
        )
        .is_ok();
        if !frame_ok {
            frame = window;
        }
        let (ww, wh) = (window.right - window.left, window.bottom - window.top);
        let (cx, cy) = (frame.left - window.left, frame.top - window.top);
        let (cw, ch) = (frame.right - frame.left, frame.bottom - frame.top);
        if cw <= 0 || ch <= 0 || ww <= 0 || wh <= 0 {
            return None;
        }
        let (tw, th) = (cw.min(max_w), ((ch as i64 * cw.min(max_w) as i64) / cw as i64).max(1) as i32);

        let screen = GetDC(None);
        let pixels = (|| {
            let full = Dib::new(screen, ww, wh)?;
            if !PrintWindow(h, full.dc, PW_RENDERFULLCONTENT).as_bool() {
                return None;
            }
            let small = Dib::new(screen, tw, th)?;
            // HALFTONE : réduction de bonne qualité (moyenne des pixels, pas de crénelage).
            SetStretchBltMode(small.dc, HALFTONE);
            let _ = SetBrushOrgEx(small.dc, 0, 0, None);
            let _ = StretchBlt(small.dc, 0, 0, tw, th, full.dc, cx, cy, cw, ch, SRCCOPY);
            let _ = GdiFlush();
            // GDI n'écrit pas l'alpha : l'image est opaque.
            Some(small.pixels().iter().map(|p| p | 0xFF00_0000).collect::<Vec<u32>>())
        })();
        ReleaseDC(None, screen);

        let px = pixels?;
        // Image entièrement noire : la capture a échoué (contenu protégé, appli figée…).
        if px.iter().all(|p| p & 0x00FF_FFFF == 0) {
            return None;
        }
        let mut image = Image { w: tw, h: th, px };
        // Coins de 8 px à 100 %, comme Windows 11, ramenés à la taille de l'image.
        let dpi = GetDpiForWindow(h).max(96) as f32;
        image.round_corners((8.0 * dpi / 96.0 * tw as f32 / cw as f32).max(tw as f32 / 32.0));
        Some(image)
    }
}
