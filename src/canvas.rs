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

    /// Image deux fois plus petite (moyenne de chaque carré de 2×2 pixels).
    pub fn half(&self) -> Image {
        let (w, h) = ((self.w / 2).max(1), (self.h / 2).max(1));
        let mut px = vec![0u32; (w * h) as usize];
        for y in 0..h {
            for x in 0..w {
                let at = |dx: i32, dy: i32| self.px[(((2 * y + dy).min(self.h - 1)) * self.w + (2 * x + dx).min(self.w - 1)) as usize];
                let quad = [at(0, 0), at(1, 0), at(0, 1), at(1, 1)];
                px[(y * w + x) as usize] = (0..4).fold(0, |acc, c| {
                    let sum: u32 = quad.iter().map(|p| (p >> (c * 8)) & 0xFF).sum();
                    acc | ((sum + 2) / 4) << (c * 8)
                });
            }
        }
        Image { w, h, px }
    }

    /// Flou (trois passes de flou « boîte », proche d'un flou gaussien).
    pub fn blur(&mut self, radius: i32) {
        if radius <= 0 || self.w <= 0 || self.h <= 0 {
            return;
        }
        let (w, h) = (self.w as usize, self.h as usize);
        let mut tmp = vec![0u32; self.px.len()];
        for _ in 0..3 {
            // Horizontal (h lignes de w pixels), puis vertical (w colonnes de h pixels).
            box_blur(&self.px, &mut tmp, h, w, 1, w, radius as usize);
            box_blur(&tmp, &mut self.px, w, h, w, 1, radius as usize);
        }
    }

    /// Lecture bilinéaire, en pixels de l'image.
    pub fn sample(&self, x: f32, y: f32) -> u32 {
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

    /// Remplit un polygone convexe (sommets dans l'ordre), bords lissés par suréchantillonnage.
    pub fn fill_convex(&mut self, points: &[(f32, f32)], rgb: (u8, u8, u8), alpha: u8) {
        if points.len() < 3 {
            return;
        }
        // Orientation du polygone, pour savoir de quel côté de chaque bord est l'intérieur.
        let area: f32 = (0..points.len())
            .map(|i| {
                let (a, b) = (points[i], points[(i + 1) % points.len()]);
                a.0 * b.1 - b.0 * a.1
            })
            .sum();
        let sign = area.signum();
        let inside = |x: f32, y: f32| {
            (0..points.len()).all(|i| {
                let (a, b) = (points[i], points[(i + 1) % points.len()]);
                ((b.0 - a.0) * (y - a.1) - (b.1 - a.1) * (x - a.0)) * sign >= 0.0
            })
        };
        let x0 = (points.iter().map(|p| p.0).fold(f32::MAX, f32::min).floor() as i32).max(0);
        let x1 = (points.iter().map(|p| p.0).fold(f32::MIN, f32::max).ceil() as i32).min(self.w);
        let y0 = (points.iter().map(|p| p.1).fold(f32::MAX, f32::min).floor() as i32).max(0);
        let y1 = (points.iter().map(|p| p.1).fold(f32::MIN, f32::max).ceil() as i32).min(self.h);
        const N: usize = 4;
        for y in y0..y1 {
            for x in x0..x1 {
                let hits = (0..N * N)
                    .filter(|k| inside(x as f32 + (k % N) as f32 / N as f32 + 0.125, y as f32 + (k / N) as f32 / N as f32 + 0.125))
                    .count();
                if hits > 0 {
                    let a = (alpha as usize * hits / (N * N)) as u8;
                    let i = (y * self.w + x) as usize;
                    self.px[i] = over(premul(rgb, a), self.px[i]);
                }
            }
        }
    }

    /// Arc de cercle ouvert vers la droite (les ondes d'un haut-parleur), bords lissés.
    /// `half_angle` : demi-ouverture en radians ; `width` : épaisseur du trait.
    pub fn arc(&mut self, center: (f32, f32), radius: f32, half_angle: f32, width: f32, rgb: (u8, u8, u8), alpha: u8) {
        let reach = radius + width;
        let x0 = ((center.0 - reach).floor() as i32).max(0);
        let x1 = ((center.0 + reach).ceil() as i32).min(self.w);
        let y0 = ((center.1 - reach).floor() as i32).max(0);
        let y1 = ((center.1 + reach).ceil() as i32).min(self.h);
        for y in y0..y1 {
            for x in x0..x1 {
                let (dx, dy) = (x as f32 + 0.5 - center.0, y as f32 + 0.5 - center.1);
                let d = dx.hypot(dy);
                let ring = (width / 2.0 - (d - radius).abs() + 0.5).clamp(0.0, 1.0);
                // Extrémités arrondies en douceur : on mesure le dépassement de l'angle en pixels.
                let ends = ((half_angle - dy.atan2(dx).abs()) * d + 0.5).clamp(0.0, 1.0);
                let cover = ring * ends;
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

/// Flou « boîte » sur les lignes : `step` sépare deux pixels d'une ligne, `stride` deux lignes.
fn box_blur(src: &[u32], dst: &mut [u32], lines: usize, len: usize, step: usize, stride: usize, r: usize) {
    let window = (2 * r + 1) as u32;
    for line in 0..lines {
        let base = line * stride;
        let at = |i: isize| src[base + (i.clamp(0, len as isize - 1) as usize) * step];
        let mut sum = [0u32; 4];
        for i in -(r as isize)..=(r as isize) {
            let p = at(i);
            for (c, s) in sum.iter_mut().enumerate() {
                *s += (p >> (c * 8)) & 0xFF;
            }
        }
        for i in 0..len {
            dst[base + i * step] =
                sum.iter().enumerate().fold(0, |acc, (c, s)| acc | ((s / window) & 0xFF) << (c * 8));
            let (add, sub) = (at(i as isize + r as isize + 1), at(i as isize - r as isize));
            for (c, s) in sum.iter_mut().enumerate() {
                *s = *s + ((add >> (c * 8)) & 0xFF) - ((sub >> (c * 8)) & 0xFF);
            }
        }
    }
}

/// Réglages de l'effet « verre liquide ».
pub struct Glass {
    /// Part de blanc mêlée au fond (0 : aucun, 1 : blanc).
    pub frost: f32,
    /// Assombrissement du fond, pour que le texte blanc reste lisible.
    pub darken: f32,
    /// Déviation de l'image près des bords, en pixels (effet de lentille).
    pub refraction: f32,
    /// Largeur de la zone courbée le long du bord, en pixels.
    pub bevel: f32,
    /// Intensité du liseré lumineux.
    pub rim: f32,
}

impl Canvas {
    /// Pose une plaque de verre en `r` (coins arrondis) : `backdrop`, ce qu'il y a derrière la
    /// fenêtre, déjà flouté, y est vu à travers une lentille. `origin` : position du canevas
    /// dans `backdrop`.
    pub fn glass(&mut self, r: RECT, radius: f32, backdrop: &Image, origin: (i32, i32), look: &Glass) {
        let (ox, oy) = (origin.0 as f32, origin.1 as f32);
        let (cx, cy) = ((r.left + r.right) as f32 / 2.0, (r.top + r.bottom) as f32 / 2.0);
        let (hx, hy) = ((r.right - r.left) as f32 / 2.0, (r.bottom - r.top) as f32 / 2.0);
        let rad = radius.min(hx).min(hy);
        for y in (r.top - 1).max(0)..(r.bottom + 1).min(self.h) {
            for x in (r.left - 1).max(0)..(r.right + 1).min(self.w) {
                let (px, py) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
                // Distance signée au bord du rectangle arrondi (négative à l'intérieur).
                let (qx, qy) = (px.abs() - (hx - rad), py.abs() - (hy - rad));
                let outside = qx.max(0.0).hypot(qy.max(0.0));
                let sd = outside + qx.max(qy).min(0.0) - rad;
                let cover = (0.5 - sd).clamp(0.0, 1.0);
                if cover <= 0.0 {
                    continue;
                }
                let depth = (-sd).max(0.0);
                // Normale au bord, vers l'extérieur.
                let (nx, ny) = if qx > 0.0 && qy > 0.0 {
                    (qx / outside.max(1e-3) * px.signum(), qy / outside.max(1e-3) * py.signum())
                } else if qx > qy {
                    (px.signum(), 0.0)
                } else {
                    (0.0, py.signum())
                };
                // Réfraction : près du bord, l'image est prise plus vers le centre, comme à
                // travers l'épaisseur arrondie d'un verre.
                let t = (1.0 - depth / look.bevel).clamp(0.0, 1.0);
                let bend = t * t * look.refraction;
                let src = backdrop.sample(x as f32 + ox - nx * bend, y as f32 + oy - ny * bend);
                let ch = |shift: u32| ((src >> shift) & 0xFF) as f32;
                let (mut rr, mut gg, mut bb) = (ch(16), ch(8), ch(0));
                // Couleurs un peu plus vives, fond assombri puis givré.
                let grey = (rr + gg + bb) / 3.0;
                for c in [&mut rr, &mut gg, &mut bb] {
                    *c = (grey + (*c - grey) * 1.25) * (1.0 - look.darken);
                    *c += (255.0 - *c) * look.frost;
                }
                // Liseré : brillant côté haut-gauche (la lumière), plus sombre en bas à droite,
                // plus un léger reflet sur le haut de la plaque.
                let edge = (1.0 - depth / 1.6).clamp(0.0, 1.0);
                let lit = (-(nx * 0.6 + ny * 0.8)).max(0.0);
                let unlit = (nx * 0.6 + ny * 0.8).max(0.0);
                // Reflet du haut : fort près du bord supérieur, il s'estompe vers le milieu.
                let top = (1.0 - (py + hy) / hy).max(0.0);
                let shine = edge * look.rim * (0.3 + 0.7 * lit) + top * top * 0.16;
                let shade = edge * 0.3 * unlit;
                for c in [&mut rr, &mut gg, &mut bb] {
                    *c = (*c + (255.0 - *c) * shine) * (1.0 - shade);
                }
                let color = (rr.clamp(0.0, 255.0) as u8, gg.clamp(0.0, 255.0) as u8, bb.clamp(0.0, 255.0) as u8);
                let i = (y * self.w + x) as usize;
                self.px[i] = over(premul(color, (255.0 * cover) as u8), self.px[i]);
            }
        }
    }
}

/// Arrondit les coins d'un polygone convexe : chaque sommet devient une petite courbe de
/// rayon `radius` (en pixels).
pub fn round_corners(points: &[(f32, f32)], radius: f32) -> Vec<(f32, f32)> {
    let n = points.len();
    let mut out = Vec::with_capacity(n * 7);
    for i in 0..n {
        let (p, v, q) = (points[(i + n - 1) % n], points[i], points[(i + 1) % n]);
        let toward = |a: (f32, f32), len: f32| {
            let (dx, dy) = (a.0 - v.0, a.1 - v.1);
            let d = dx.hypot(dy).max(1e-3);
            let r = len.min(d / 2.0);
            (v.0 + dx / d * r, v.1 + dy / d * r)
        };
        let (a, b) = (toward(p, radius), toward(q, radius));
        // Courbe de Bézier quadratique a → (v) → b.
        for k in 0..=6 {
            let t = k as f32 / 6.0;
            let u = 1.0 - t;
            out.push((u * u * a.0 + 2.0 * u * t * v.0 + t * t * b.0, u * u * a.1 + 2.0 * u * t * v.1 + t * t * b.1));
        }
    }
    out
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
    fn le_flou_adoucit_un_contraste() {
        let mut img = Image { w: 9, h: 1, px: vec![0xFF00_0000; 9] };
        img.px[4] = 0xFFFF_FFFF;
        img.blur(2);
        let red = |i: usize| channel(img.px[i], 16);
        assert!(red(4) < 255 && red(4) > 0, "le point blanc s'étale");
        assert!(red(3) > 0 && red(5) > 0, "ses voisins s'éclaircissent");
        assert_eq!(img.px[0] >> 24, 255, "l'alpha reste plein");
    }

    #[test]
    fn plaque_de_verre() {
        let backdrop = Image { w: 40, h: 40, px: vec![premul((0, 100, 0), 255); 1600] };
        let mut c = Canvas::new(40, 40);
        let look = Glass { frost: 0.1, darken: 0.2, refraction: 4.0, bevel: 6.0, rim: 0.6 };
        c.glass(RECT { left: 5, top: 5, right: 35, bottom: 35 }, 8.0, &backdrop, (0, 0), &look);
        assert_eq!(c.px[(20 * 40 + 20) as usize] >> 24, 255, "le centre est couvert");
        assert_eq!(c.px[(5 * 40 + 5) as usize] >> 24, 0, "le coin arrondi reste vide");
        assert!(channel(c.px[(20 * 40 + 20) as usize], 8) > 0, "on voit le fond à travers");
    }

    #[test]
    fn triangle_lisse() {
        let mut c = Canvas::new(20, 20);
        let tri = round_corners(&[(4.0, 2.0), (18.0, 10.0), (4.0, 18.0)], 2.0);
        c.fill_convex(&tri, (255, 255, 255), 255);
        assert_eq!(c.px[(10 * 20 + 8) as usize] >> 24, 255, "l'intérieur est plein");
        assert_eq!(c.px[(2 * 20 + 16) as usize] >> 24, 0, "l'extérieur est vide");
        let edge = (0..20).map(|x| c.px[(10 * 20 + x) as usize] >> 24).filter(|a| *a > 0 && *a < 255).count();
        assert!(edge > 0, "les bords sont lissés");
    }

    #[test]
    fn coins_arrondis_lisses() {
        let mut c = Canvas::new(20, 20);
        c.round_rect(RECT { left: 0, top: 0, right: 20, bottom: 20 }, 8, (255, 255, 255), 255);
        assert_eq!(c.px[0] >> 24, 0, "le coin est vide");
        assert_eq!(c.px[(10 * 20 + 10) as usize] >> 24, 255, "le centre est plein");
    }
}
