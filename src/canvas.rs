//! Dessin avec transparence par pixel, pour la barre latérale (fenêtre « en couches »).

use std::{ffi::c_void, mem::size_of};

use windows::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    UI::WindowsAndMessaging::{UpdateLayeredWindow, ULW_ALPHA},
};

/// Petite image en BGRA, alpha prémultiplié (instantané d'une fenêtre).
pub struct Image {
    pub w: i32,
    pub h: i32,
    pub px: Vec<u32>,
}

impl Image {
    /// Arrondit les coins, comme les fenêtres de Windows 11.
    pub fn round_corners(&mut self, radius: f32) {
        let r = radius.min(self.w as f32 / 2.0).min(self.h as f32 / 2.0);
        let ri = r.ceil() as i32;
        for y in 0..self.h {
            for x in 0..self.w {
                if (x >= ri && x < self.w - ri) || (y >= ri && y < self.h - ri) {
                    continue;
                }
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let d = (px - px.clamp(r, self.w as f32 - r)).hypot(py - py.clamp(r, self.h as f32 - r));
                let cover = (r - d + 0.5).clamp(0.0, 1.0);
                let i = (y * self.w + x) as usize;
                self.px[i] = scale(self.px[i], cover);
            }
        }
    }

    /// Lecture bilinéaire, en pixels de l'image.
    fn sample(&self, x: f32, y: f32) -> u32 {
        let x = x.clamp(0.0, (self.w - 1) as f32);
        let y = y.clamp(0.0, (self.h - 1) as f32);
        let (x0, y0) = (x as i32, y as i32);
        let (x1, y1) = ((x0 + 1).min(self.w - 1), (y0 + 1).min(self.h - 1));
        let (fx, fy) = (x - x0 as f32, y - y0 as f32);
        let at = |x: i32, y: i32| self.px[(y * self.w + x) as usize];
        let (a, b, c, d) = (at(x0, y0), at(x1, y0), at(x0, y1), at(x1, y1));
        let mix = |shift: u32| {
            let ch = |p: u32| ((p >> shift) & 0xFF) as f32;
            let top = ch(a) + (ch(b) - ch(a)) * fx;
            let bottom = ch(c) + (ch(d) - ch(c)) * fx;
            ((top + (bottom - top) * fy).round() as u32).min(255) << shift
        };
        mix(24) | mix(16) | mix(8) | mix(0)
    }
}

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

    /// Dessine `img` déformée dans le quadrilatère `quad` (haut-gauche, haut-droit, bas-droit,
    /// bas-gauche) : c'est ce qui donne la perspective. `shade` assombrit (0 : rien, 255 : noir).
    pub fn draw_quad(&mut self, img: &Image, quad: [(f32, f32); 4], alpha: u8, shade: u8) {
        if alpha == 0 || img.w <= 0 || img.h <= 0 {
            return;
        }
        let Some(inv) = Homography::square_to_quad(quad).inverse() else { return };
        let xs = quad.map(|p| p.0);
        let ys = quad.map(|p| p.1);
        let x0 = (xs.iter().copied().fold(f32::MAX, f32::min).floor() as i32).max(0);
        let x1 = (xs.iter().copied().fold(f32::MIN, f32::max).ceil() as i32).min(self.w);
        let y0 = (ys.iter().copied().fold(f32::MAX, f32::min).floor() as i32).max(0);
        let y1 = (ys.iter().copied().fold(f32::MIN, f32::max).ceil() as i32).min(self.h);
        // Taille du quadrilatère, pour lisser ses bords sur un pixel environ.
        let dist = |a: (f32, f32), b: (f32, f32)| (a.0 - b.0).hypot(a.1 - b.1);
        let qw = dist(quad[0], quad[1]).max(dist(quad[3], quad[2]));
        let qh = dist(quad[0], quad[3]).max(dist(quad[1], quad[2]));
        let edge = |t: f32, len: f32| (t.min(1.0 - t) * len + 0.5).clamp(0.0, 1.0);
        let opacity = alpha as f32 / 255.0;
        let light = (255 - shade) as u32;
        for y in y0..y1 {
            for x in x0..x1 {
                let Some((u, v)) = inv.apply(x as f32 + 0.5, y as f32 + 0.5) else { continue };
                let cover = edge(u, qw) * edge(v, qh);
                if cover <= 0.0 {
                    continue;
                }
                let mut src = img.sample(u * img.w as f32 - 0.5, v * img.h as f32 - 0.5);
                if shade > 0 {
                    let dim = |shift: u32| (((src >> shift) & 0xFF) * light / 255) << shift;
                    src = (src & 0xFF00_0000) | dim(16) | dim(8) | dim(0);
                }
                let i = (y * self.w + x) as usize;
                self.px[i] = over(scale(src, cover * opacity), self.px[i]);
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

/// Transformation projective du carré unité vers un quadrilatère (et retour).
struct Homography([f32; 9]);

impl Homography {
    /// Coins dans l'ordre : (0,0), (1,0), (1,1), (0,1).
    fn square_to_quad(q: [(f32, f32); 4]) -> Self {
        let [(x0, y0), (x1, y1), (x2, y2), (x3, y3)] = q;
        let (sx, sy) = (x0 - x1 + x2 - x3, y0 - y1 + y2 - y3);
        if sx.abs() < 1e-6 && sy.abs() < 1e-6 {
            // Parallélogramme : transformation affine.
            return Self([x1 - x0, x3 - x0, x0, y1 - y0, y3 - y0, y0, 0.0, 0.0, 1.0]);
        }
        let (dx1, dx2, dy1, dy2) = (x1 - x2, x3 - x2, y1 - y2, y3 - y2);
        let det = dx1 * dy2 - dx2 * dy1;
        let g = (sx * dy2 - dx2 * sy) / det;
        let h = (dx1 * sy - sx * dy1) / det;
        Self([x1 - x0 + g * x1, x3 - x0 + h * x3, x0, y1 - y0 + g * y1, y3 - y0 + h * y3, y0, g, h, 1.0])
    }

    fn inverse(&self) -> Option<Self> {
        let [a, b, c, d, e, f, g, h, i] = self.0;
        let (ca, cb, cc) = (e * i - f * h, f * g - d * i, d * h - e * g);
        let det = a * ca + b * cb + c * cc;
        if det.abs() < 1e-9 {
            return None;
        }
        let m = [ca, c * h - b * i, b * f - c * e, cb, a * i - c * g, c * d - a * f, cc, b * g - a * h, a * e - b * d];
        Some(Self(m.map(|v| v / det)))
    }

    fn apply(&self, x: f32, y: f32) -> Option<(f32, f32)> {
        let m = &self.0;
        let w = m[6] * x + m[7] * y + m[8];
        (w.abs() > 1e-9).then(|| ((m[0] * x + m[1] * y + m[2]) / w, (m[3] * x + m[4] * y + m[5]) / w))
    }
}

/// Multiplie un pixel prémultiplié par `k` (entre 0 et 1).
fn scale(px: u32, k: f32) -> u32 {
    let mul = |shift: u32| ((((px >> shift) & 0xFF) as f32 * k).round() as u32).min(255) << shift;
    mul(24) | mul(16) | mul(8) | mul(0)
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
pub(crate) struct Dib {
    pub dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u32,
    len: usize,
}

impl Dib {
    pub unsafe fn new(screen: HDC, w: i32, h: i32) -> Option<Self> {
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

    #[allow(clippy::mut_from_ref)]
    pub fn pixels(&self) -> &mut [u32] {
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
    fn perspective_du_carre_vers_un_trapeze() {
        // Bord gauche plus court, comme une carte tournée vers le centre de l'écran.
        let quad = [(0.0, 10.0), (100.0, 0.0), (100.0, 80.0), (0.0, 70.0)];
        let hom = Homography::square_to_quad(quad);
        let inv = hom.inverse().unwrap();
        for (u, v) in [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0), (0.3, 0.6)] {
            let (x, y) = hom.apply(u, v).unwrap();
            let (u2, v2) = inv.apply(x, y).unwrap();
            assert!((u - u2).abs() < 1e-4 && (v - v2).abs() < 1e-4);
        }
        let (x, y) = hom.apply(0.0, 1.0).unwrap();
        assert!((x - 0.0).abs() < 1e-3 && (y - 70.0).abs() < 1e-3);
    }

    #[test]
    fn image_dessinee_en_perspective() {
        let img = Image { w: 4, h: 4, px: vec![premul((255, 0, 0), 255); 16] };
        let mut c = Canvas::new(40, 40);
        c.draw_quad(&img, [(5.0, 10.0), (35.0, 5.0), (35.0, 35.0), (5.0, 30.0)], 255, 0);
        assert_eq!(c.px[(20 * 40 + 20) as usize], premul((255, 0, 0), 255), "le centre est rempli");
        assert_eq!(c.px[(6 * 40 + 6) as usize] >> 24, 0, "le coin rogné par la perspective est vide");
    }

    #[test]
    fn coins_arrondis_lisses() {
        let mut c = Canvas::new(20, 20);
        c.round_rect(RECT { left: 0, top: 0, right: 20, bottom: 20 }, 8, (255, 255, 255), 255);
        assert_eq!(c.px[0] >> 24, 0, "le coin est vide");
        assert_eq!(c.px[(10 * 20 + 10) as usize] >> 24, 255, "le centre est plein");
    }
}
