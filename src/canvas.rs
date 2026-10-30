//! Dessin avec transparence par pixel, pour la barre latérale (fenêtre « en couches »).

use std::{ffi::c_void, mem::size_of};

use windows::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    UI::WindowsAndMessaging::{UpdateLayeredWindow, ULW_ALPHA},
};

/// Image en BGRA, alpha prémultiplié (le format qu'attend UpdateLayeredWindow).
pub struct Canvas {
    w: i32,
    h: i32,
    px: Vec<u32>,
}

impl Canvas {
    pub fn new(w: i32, h: i32) -> Self {
        Self { w, h, px: vec![0; (w.max(0) * h.max(0)) as usize] }
    }

    pub fn clear(&mut self, rgb: (u8, u8, u8), alpha: u8) {
        self.px.fill(premul(rgb, alpha));
    }

    /// Rectangle arrondi translucide posé par-dessus, avec des coins lissés.
    pub fn round_rect(&mut self, r: RECT, radius: i32, rgb: (u8, u8, u8), alpha: u8) {
        let rad = radius.min((r.right - r.left) / 2).min((r.bottom - r.top) / 2).max(0) as f32;
        let (left, top, right, bottom) = (r.left as f32 + rad, r.top as f32 + rad, r.right as f32 - rad, r.bottom as f32 - rad);
        for y in r.top.max(0)..r.bottom.min(self.h) {
            for x in r.left.max(0)..r.right.min(self.w) {
                // Distance au centre de l'arrondi le plus proche (nulle hors des coins).
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let d = (px - px.clamp(left, right)).hypot(py - py.clamp(top, bottom));
                let cover = (rad - d + 0.5).clamp(0.0, 1.0);
                if cover > 0.0 {
                    let i = (y * self.w + x) as usize;
                    self.px[i] = over(premul(rgb, (alpha as f32 * cover) as u8), self.px[i]);
                }
            }
        }
    }

    /// Dessine avec GDI (texte, icônes…) par-dessus l'image. GDI n'écrit pas l'alpha :
    /// on dessine deux fois, sur noir puis sur blanc, et l'écart entre les deux donne
    /// la couverture de chaque pixel (bords du texte lissés, icônes semi-transparentes).
    pub fn gdi(&mut self, draw: impl Fn(HDC)) {
        unsafe {
            let screen = GetDC(None);
            let (Some(black), Some(white)) = (Dib::new(screen, self.w, self.h), Dib::new(screen, self.w, self.h)) else {
                ReleaseDC(None, screen);
                return;
            };
            black.pixels().fill(0x0000_0000);
            white.pixels().fill(0x00FF_FFFF);
            draw(black.dc);
            draw(white.dc);
            let _ = GdiFlush();
            for ((dst, &b), &w) in self.px.iter_mut().zip(black.pixels().iter()).zip(white.pixels().iter()) {
                let diff = (channel(w, 16) + channel(w, 8) + channel(w, 0)) - (channel(b, 16) + channel(b, 8) + channel(b, 0));
                let alpha = 255 - (diff / 3).clamp(0, 255);
                if alpha > 0 {
                    let c = |shift| channel(b, shift).min(alpha) as u32;
                    let src = (alpha as u32) << 24 | c(16) << 16 | c(8) << 8 | c(0);
                    *dst = over(src, *dst);
                }
            }
            ReleaseDC(None, screen);
        }
    }

    /// Affiche l'image dans la fenêtre en couches, à la position `pos` de l'écran.
    pub fn present(&self, hwnd: HWND, pos: POINT) {
        unsafe {
            let screen = GetDC(None);
            if let Some(dib) = Dib::new(screen, self.w, self.h) {
                dib.pixels().copy_from_slice(&self.px);
                let size = SIZE { cx: self.w, cy: self.h };
                let blend = BLENDFUNCTION {
                    BlendOp: AC_SRC_OVER as u8,
                    BlendFlags: 0,
                    SourceConstantAlpha: 255,
                    AlphaFormat: AC_SRC_ALPHA as u8,
                };
                let _ = UpdateLayeredWindow(
                    hwnd,
                    screen,
                    Some(&pos),
                    Some(&size),
                    dib.dc,
                    Some(&POINT::default()),
                    COLORREF(0),
                    Some(&blend),
                    ULW_ALPHA,
                );
            }
            ReleaseDC(None, screen);
        }
    }
}

fn channel(px: u32, shift: u32) -> i32 {
    ((px >> shift) & 0xFF) as i32
}

fn premul((r, g, b): (u8, u8, u8), a: u8) -> u32 {
    let m = |c: u8| (c as u32 * a as u32 / 255) & 0xFF;
    (a as u32) << 24 | m(r) << 16 | m(g) << 8 | m(b)
}

/// `src` par-dessus `dst`, en alpha prémultiplié.
fn over(src: u32, dst: u32) -> u32 {
    let keep = 255 - (src >> 24);
    let mix = |shift: u32| (((src >> shift) & 0xFF) + ((dst >> shift) & 0xFF) * keep / 255).min(255) << shift;
    mix(24) | mix(16) | mix(8) | mix(0)
}

/// Bitmap 32 bits sélectionné dans un DC mémoire, dont on peut lire et écrire les pixels.
struct Dib {
    dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u32,
    len: usize,
}

impl Dib {
    unsafe fn new(screen: HDC, w: i32, h: i32) -> Option<Self> {
        if w <= 0 || h <= 0 {
            return None;
        }
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // de haut en bas
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        let bmp = CreateDIBSection(screen, &info, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
        let dc = CreateCompatibleDC(screen);
        let old = SelectObject(dc, bmp);
        Some(Self { dc, bmp, old, bits: bits as *mut u32, len: (w * h) as usize })
    }

    fn pixels(&self) -> &mut [u32] {
        unsafe { std::slice::from_raw_parts_mut(self.bits, self.len) }
    }
}

impl Drop for Dib {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.bmp);
            let _ = DeleteDC(self.dc);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn superposition_en_alpha_premultiplie() {
        let opaque_red = premul((255, 0, 0), 255);
        let half_white = premul((255, 255, 255), 128);
        assert_eq!(over(opaque_red, half_white), opaque_red);
        let result = over(half_white, opaque_red);
        assert_eq!(result >> 24, 255);
        assert_eq!(channel(result, 16), 255);
        assert!((channel(result, 8) - 128).abs() <= 1);
    }

    #[test]
    fn coins_arrondis_lisses() {
        let mut c = Canvas::new(20, 20);
        c.round_rect(RECT { left: 0, top: 0, right: 20, bottom: 20 }, 8, (255, 255, 255), 255);
        assert_eq!(c.px[0] >> 24, 0, "le coin est vide");
        assert_eq!(c.px[(10 * 20 + 10) as usize] >> 24, 255, "le centre est plein");
    }
}
