//! Dessin avec transparence par pixel, pour la barre latérale (fenêtre « en couches »).

use std::{cell::RefCell, ffi::c_void, mem::size_of};

use windows::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    UI::WindowsAndMessaging::{UpdateLayeredWindow, ULW_ALPHA},
};

/// Petite image en BGRA, alpha prémultiplié (instantané d'une fenêtre).
#[derive(Clone)]
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
    /// Zone déjà peinte (gauche, haut, droite, bas) : le reste est transparent, et les calques
    /// ne sont fondus que là.
    dirty: RECT,
}

thread_local! {
    /// Tampons de pixels rendus par les canevas : pendant une animation, chaque image en
    /// redessine plusieurs de la même taille, sans repasser par l'allocateur.
    static POOL: RefCell<Vec<Vec<u32>>> = const { RefCell::new(Vec::new()) };
}
const POOL_MAX: usize = 4;
/// Zone d'un DC non vide, d'après GetBoundsRect (DCB_RESET | DCB_ACCUMULATE).
const DCB_SET: u32 = 3;

impl Canvas {
    pub fn new(w: i32, h: i32) -> Self {
        let len = (w.max(0) * h.max(0)) as usize;
        let mut px = POOL.with(|pool| pool.borrow_mut().pop()).unwrap_or_default();
        px.clear();
        px.resize(len, 0);
        Self { w, h, px, dirty: RECT::default() }
    }

    /// L'image dessinée, pour la réutiliser ailleurs (une icône à agrandir…).
    pub fn into_image(mut self) -> Image {
        Image { w: self.w, h: self.h, px: std::mem::take(&mut self.px) }
    }

    /// Agrandit la zone peinte (bornes déjà ramenées dans le canevas).
    fn touch(&mut self, left: i32, top: i32, right: i32, bottom: i32) {
        if left >= right || top >= bottom {
            return;
        }
        let d = &mut self.dirty;
        if d.left >= d.right || d.top >= d.bottom {
            *d = RECT { left, top, right, bottom };
        } else {
            *d = RECT { left: d.left.min(left), top: d.top.min(top), right: d.right.max(right), bottom: d.bottom.max(bottom) };
        }
    }

    pub fn round_rect(&mut self, r: RECT, radius: i32, rgb: (u8, u8, u8), alpha: u8) {
        if alpha == 0 {
            return;
        }
        let rad = radius.min((r.right - r.left) / 2).min((r.bottom - r.top) / 2).max(0) as f32;
        let (left, top, right, bottom) = (r.left as f32 + rad, r.top as f32 + rad, r.right as f32 - rad, r.bottom as f32 - rad);
        let (x0, x1, y0, y1) = (r.left.max(0), r.right.min(self.w), r.top.max(0), r.bottom.min(self.h));
        self.touch(x0, y0, x1, y1);
        let full = premul(rgb, alpha);
        // Pixels pleins d'une ligne hors des coins : ceux dont le centre est entre `left` et `right`.
        let (inner_left, inner_right) = (((left - 0.5).ceil() as i32).max(x0), ((right - 0.5).floor() as i32 + 1).min(x1));
        for y in y0..y1 {
            let py = y as f32 + 0.5;
            let dy = py - py.clamp(top, bottom);
            let row = (y * self.w) as usize;
            // Hors des coins, toute la ligne est pleine (à moins d'un rayon du bord, le pixel est
            // couvert) ; entre les coins, la couverture ne dépend que de la ligne.
            let (a, b) = if dy == 0.0 {
                (x0, x1)
            } else if inner_left < inner_right {
                (inner_left, inner_right)
            } else {
                (x1, x1)
            };
            if a < b {
                let color = if dy == 0.0 {
                    full
                } else {
                    let cover = (rad - dy.abs() + 0.5).clamp(0.0, 1.0);
                    if cover > 0.0 { premul(rgb, (alpha as f32 * cover).round() as u8) } else { 0 }
                };
                blend_span(&mut self.px[row + a as usize..row + b as usize], color);
            }
            for x in (x0..a.min(x1)).chain(b.max(x0)..x1) {
                // Distance au centre de l'arrondi le plus proche (nulle hors des coins).
                let px = x as f32 + 0.5;
                let dx = px - px.clamp(left, right);
                let i = row + x as usize;
                // Hors des coins, le pixel est plein ; dans un coin, on lisse le bord de l'arrondi.
                // (Sans ce cas à part, un rectangle sans arrondi n'était couvert qu'à moitié.)
                if dx == 0.0 && dy == 0.0 {
                    self.px[i] = over(full, self.px[i]);
                    continue;
                }
                let cover = (rad - dx.hypot(dy) + 0.5).clamp(0.0, 1.0);
                if cover > 0.0 {
                    self.px[i] = over(premul(rgb, (alpha as f32 * cover).round() as u8), self.px[i]);
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
        // Bords normalisés : distance signée du centre d'un pixel à chaque bord (positive dedans).
        // Les bords de longueur nulle (sommets confondus) ne limitent rien.
        let edges: Vec<(f32, f32, f32, f32)> = (0..points.len())
            .filter_map(|i| {
                let (a, b) = (points[i], points[(i + 1) % points.len()]);
                let len = (b.0 - a.0).hypot(b.1 - a.1);
                (len > 1e-6).then(|| ((b.0 - a.0) * sign / len, (b.1 - a.1) * sign / len, a.0, a.1))
            })
            .collect();
        let x0 = (points.iter().map(|p| p.0).fold(f32::MAX, f32::min).floor() as i32).max(0);
        let x1 = (points.iter().map(|p| p.0).fold(f32::MIN, f32::max).ceil() as i32).min(self.w);
        let y0 = (points.iter().map(|p| p.1).fold(f32::MAX, f32::min).floor() as i32).max(0);
        let y1 = (points.iter().map(|p| p.1).fold(f32::MIN, f32::max).ceil() as i32).min(self.h);
        self.touch(x0, y0, x1, y1);
        const N: usize = 4;
        // Les échantillons sont à moins de 0,54 pixel du centre : au-delà de cette marge, le pixel
        // est entièrement dedans (ou dehors) sans avoir à les compter.
        const MARGIN: f32 = 0.6;
        let full = premul(rgb, alpha);
        for y in y0..y1 {
            let row = (y * self.w) as usize;
            let cy = y as f32 + 0.5;
            for x in x0..x1 {
                let cx = x as f32 + 0.5;
                let nearest = edges.iter().map(|&(dx, dy, ax, ay)| dx * (cy - ay) - dy * (cx - ax)).fold(f32::MAX, f32::min);
                let i = row + x as usize;
                if nearest >= MARGIN {
                    self.px[i] = over(full, self.px[i]);
                    continue;
                }
                if nearest <= -MARGIN {
                    continue;
                }
                let hits = (0..N * N)
                    .filter(|k| inside(x as f32 + (k % N) as f32 / N as f32 + 0.125, y as f32 + (k / N) as f32 / N as f32 + 0.125))
                    .count();
                if hits > 0 {
                    let a = (alpha as usize * hits / (N * N)) as u8;
                    self.px[i] = over(premul(rgb, a), self.px[i]);
                }
            }
        }
    }

    /// Arc de cercle ouvert vers la droite (les ondes d'un haut-parleur), bords lissés.
    /// `half_angle` : demi-ouverture en radians ; `width` : épaisseur du trait.
    pub fn arc(&mut self, center: (f32, f32), radius: f32, half_angle: f32, width: f32, rgb: (u8, u8, u8), alpha: u8) {
        self.arc_toward(center, radius, 0.0, half_angle, width, rgb, alpha);
    }

    /// Arc de cercle ouvert dans la direction `toward` (en radians, 0 vers la droite, -PI/2 vers
    /// le haut), comme `arc`.
    #[allow(clippy::too_many_arguments)]
    pub fn arc_toward(&mut self, center: (f32, f32), radius: f32, toward: f32, half_angle: f32, width: f32, rgb: (u8, u8, u8), alpha: u8) {
        use std::f32::consts::{PI, TAU};
        let reach = radius + width;
        let x0 = ((center.0 - reach).floor() as i32).max(0);
        let x1 = ((center.0 + reach).ceil() as i32).min(self.w);
        let y0 = ((center.1 - reach).floor() as i32).max(0);
        let y1 = ((center.1 + reach).ceil() as i32).min(self.h);
        self.touch(x0, y0, x1, y1);
        for y in y0..y1 {
            for x in x0..x1 {
                let (dx, dy) = (x as f32 + 0.5 - center.0, y as f32 + 0.5 - center.1);
                let d = dx.hypot(dy);
                let ring = (width / 2.0 - (d - radius).abs() + 0.5).clamp(0.0, 1.0);
                // Extrémités arrondies en douceur : on mesure le dépassement de l'angle en pixels.
                let angle = (dy.atan2(dx) - toward + PI).rem_euclid(TAU) - PI;
                let ends = ((half_angle - angle.abs()) * d + 0.5).clamp(0.0, 1.0);
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
        let (iw, ih) = (img.w as f32, img.h as f32);
        // Image réduite (une icône de 128 px dessinée sur 20) : un seul échantillon par pixel
        // sauterait des pans entiers de l'image et crénelerait le dessin. On moyenne alors
        // n × n échantillons répartis sur le pixel, de quoi couvrir tout ce qu'il recouvre.
        let n = ((iw / qw.max(1.0)).max(ih / qh.max(1.0)).ceil() as i32).clamp(1, 8);
        let steps: Vec<f32> = (0..n).map(|k| (k as f32 + 0.5) / n as f32).collect();
        let sample_at = |x: f32, y: f32| inv.apply(x, y).map(|(u, v)| img.sample(u * iw - 0.5, v * ih - 0.5));
        self.touch(x0, y0, x1, y1);
        for y in y0..y1 {
            let row = (y * self.w) as usize;
            let cy = y as f32 + 0.5;
            for x in x0..x1 {
                let Some((u, v)) = inv.apply(x as f32 + 0.5, cy) else { continue };
                let cover = edge(u, qw) * edge(v, qh);
                if cover <= 0.0 {
                    continue;
                }
                let mut src = if n == 1 {
                    img.sample(u * iw - 0.5, v * ih - 0.5)
                } else {
                    let mut sum = [0u32; 4];
                    for &dy in &steps {
                        for &dx in &steps {
                            let p = sample_at(x as f32 + dx, y as f32 + dy).unwrap_or(0);
                            for (c, s) in sum.iter_mut().enumerate() {
                                *s += (p >> (8 * c)) & 0xFF;
                            }
                        }
                    }
                    let count = (n * n) as u32;
                    sum.iter().enumerate().fold(0, |acc, (c, s)| acc | ((s + count / 2) / count) << (8 * c))
                };
                if src == 0 {
                    continue;
                }
                if shade > 0 {
                    let dim = |shift: u32| (((src >> shift) & 0xFF) * light / 255) << shift;
                    src = (src & 0xFF00_0000) | dim(16) | dim(8) | dim(0);
                }
                let i = row + x as usize;
                self.px[i] = over(scale(src, cover * opacity), self.px[i]);
            }
        }
    }

    /// Dessine avec GDI (texte, icônes…) par-dessus l'image. GDI n'écrit pas l'alpha :
    /// on dessine deux fois, sur noir puis sur blanc, et l'écart entre les deux donne
    /// la couverture de chaque pixel (bords du texte lissés, icônes semi-transparentes).
    /// Les deux bitmaps sont gardés d'une fois sur l'autre, et seule la zone où GDI a dessiné
    /// (il la tient à jour) est relue puis remise à neuf.
    pub fn gdi(&mut self, draw: impl Fn(HDC)) {
        if self.w <= 0 || self.h <= 0 {
            return;
        }
        GDI_PAIR.with(|cell| {
            let mut cell = cell.borrow_mut();
            if cell.as_ref().is_none_or(|p| p.w < self.w || p.h < self.h) {
                let (w, h) = cell.as_ref().map_or((self.w, self.h), |p| (p.w.max(self.w), p.h.max(self.h)));
                *cell = None;
                *cell = GdiPair::new(w, h);
            }
            let Some(pair) = cell.as_mut() else { return };
            let whole = RECT { left: 0, top: 0, right: self.w, bottom: self.h };
            let mut drawn: Option<RECT> = None;
            unsafe {
                for dib in [&pair.black, &pair.white] {
                    let saved = SaveDC(dib.dc);
                    // Le bitmap peut être plus grand que le canevas : rien ne doit déborder.
                    IntersectClipRect(dib.dc, 0, 0, self.w, self.h);
                    SetBoundsRect(dib.dc, None, SET_BOUNDS_RECT_FLAGS(DCB_RESET.0 | DCB_ENABLE.0));
                    draw(dib.dc);
                    let mut r = RECT::default();
                    let state = GetBoundsRect(dib.dc, &mut r, 0) & DCB_SET;
                    SetBoundsRect(dib.dc, None, SET_BOUNDS_RECT_FLAGS(DCB_RESET.0 | DCB_DISABLE.0));
                    let _ = RestoreDC(dib.dc, saved);
                    let r = if state == DCB_RESET.0 {
                        // Rien de dessiné.
                        continue;
                    } else if state == DCB_SET {
                        r
                    } else {
                        // Zone inconnue : on relit tout le canevas.
                        whole
                    };
                    drawn = Some(drawn.map_or(r, |d| union(d, r)));
                }
                let _ = GdiFlush();
            }
            let Some(r) = drawn.map(|r| intersect(r, whole)).filter(|r| r.left < r.right && r.top < r.bottom) else { return };
            self.touch(r.left, r.top, r.right, r.bottom);
            let (black, white) = (pair.black.pixels(), pair.white.pixels());
            for y in r.top..r.bottom {
                let (src_row, dst_row) = ((y * pair.w) as usize, (y * self.w) as usize);
                for x in r.left as usize..r.right as usize {
                    let (b, w) = (black[src_row + x], white[src_row + x]);
                    black[src_row + x] = 0x0000_0000;
                    white[src_row + x] = 0x00FF_FFFF;
                    let diff = (channel(w, 16) + channel(w, 8) + channel(w, 0)) - (channel(b, 16) + channel(b, 8) + channel(b, 0));
                    let alpha = 255 - (diff / 3).clamp(0, 255);
                    if alpha > 0 {
                        let c = |shift| channel(b, shift).min(alpha) as u32;
                        let src = (alpha as u32) << 24 | c(16) << 16 | c(8) << 8 | c(0);
                        let dst = &mut self.px[dst_row + x];
                        *dst = over(src, *dst);
                    }
                }
            }
        });
    }

    /// Affiche l'image dans la fenêtre en couches, à la position `pos` de l'écran. Le bitmap
    /// passé à Windows est gardé d'une image à l'autre (agrandi au besoin).
    pub fn present(&self, hwnd: HWND, pos: POINT) {
        if self.w <= 0 || self.h <= 0 {
            return;
        }
        SURFACE.with(|cell| {
            let mut cell = cell.borrow_mut();
            if cell.as_ref().is_none_or(|d| d.w < self.w || d.h < self.h) {
                let (w, h) = cell.as_ref().map_or((self.w, self.h), |d| (d.w.max(self.w), d.h.max(self.h)));
                *cell = None;
                *cell = unsafe { Dib::new(w, h) };
            }
            let Some(dib) = cell.as_ref() else { return };
            let pixels = dib.pixels();
            let (w, stride) = (self.w as usize, dib.w as usize);
            for (y, row) in self.px.chunks_exact(w).enumerate() {
                pixels[y * stride..y * stride + w].copy_from_slice(row);
            }
            let size = SIZE { cx: self.w, cy: self.h };
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            unsafe {
                let _ = UpdateLayeredWindow(
                    hwnd,
                    None,
                    Some(&pos),
                    Some(&size),
                    dib.dc,
                    Some(&POINT::default()),
                    COLORREF(0),
                    Some(&blend),
                    ULW_ALPHA,
                );
            }
        });
    }
}

impl Drop for Canvas {
    fn drop(&mut self) {
        let px = std::mem::take(&mut self.px);
        if px.capacity() > 0 {
            POOL.with(|pool| {
                let mut pool = pool.borrow_mut();
                if pool.len() < POOL_MAX {
                    pool.push(px);
                }
            });
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

/// Matière des panneaux, à la manière de macOS (Big Sur à Sonoma, mode sombre) : un gris foncé
/// presque plein, à peine teinté par le fond flouté, avec un fin liseré clair.
pub struct Material {
    pub tint: (u8, u8, u8),
    /// Part de la teinte par-dessus le fond (1 : entièrement plein).
    pub opacity: f32,
    /// Opacité du liseré clair qui fait le tour du panneau.
    pub border: f32,
}

impl Canvas {
    /// Pose un panneau plein en `r` (coins arrondis). `backdrop`, ce qu'il y a derrière la
    /// fenêtre (déjà flouté), transparaît à peine ; `origin` : position du canevas dans `backdrop`.
    /// Sans `backdrop`, le panneau est simplement de la couleur de la teinte.
    pub fn material(&mut self, r: RECT, radius: f32, backdrop: Option<&Image>, origin: (i32, i32), look: &Material) {
        let (ox, oy) = (origin.0 as f32, origin.1 as f32);
        let (cx, cy) = ((r.left + r.right) as f32 / 2.0, (r.top + r.bottom) as f32 / 2.0);
        let (hx, hy) = ((r.right - r.left) as f32 / 2.0, (r.bottom - r.top) as f32 / 2.0);
        let rad = radius.min(hx).min(hy);
        let tint = [look.tint.0 as f32, look.tint.1 as f32, look.tint.2 as f32];
        // Sans fond, l'intérieur du panneau (hors liseré) est d'une seule couleur.
        let inner = premul(look.tint, 255);
        let (x0, x1, y0, y1) = ((r.left - 1).max(0), (r.right + 1).min(self.w), (r.top - 1).max(0), (r.bottom + 1).min(self.h));
        self.touch(x0, y0, x1, y1);
        for y in y0..y1 {
            let py = y as f32 + 0.5 - cy;
            let qy = py.abs() - (hy - rad);
            for x in x0..x1 {
                let px = x as f32 + 0.5 - cx;
                // Distance signée au bord du rectangle arrondi (négative à l'intérieur).
                let qx = px.abs() - (hx - rad);
                let sd = outside(qx, qy) + qx.max(qy).min(0.0) - rad;
                let cover = (0.5 - sd).clamp(0.0, 1.0);
                if cover <= 0.0 {
                    continue;
                }
                if backdrop.is_none() && sd <= -1.0 {
                    let i = (y * self.w + x) as usize;
                    self.px[i] = over(inner, self.px[i]);
                    continue;
                }
                let behind = match backdrop {
                    Some(b) => {
                        let p = b.sample(x as f32 + ox, y as f32 + oy);
                        [((p >> 16) & 0xFF) as f32, ((p >> 8) & 0xFF) as f32, (p & 0xFF) as f32]
                    }
                    None => tint,
                };
                // Liseré d'un pixel le long du bord.
                let edge = (1.0 - (-sd).max(0.0)).clamp(0.0, 1.0) * look.border;
                let mix = |c: usize| {
                    let solid = behind[c] + (tint[c] - behind[c]) * look.opacity;
                    (solid + (255.0 - solid) * edge).clamp(0.0, 255.0) as u8
                };
                let i = (y * self.w + x) as usize;
                self.px[i] = over(premul((mix(0), mix(1), mix(2)), (255.0 * cover) as u8), self.px[i]);
            }
        }
    }

    /// Encoche des MacBook Pro, collée au haut de `r` : coins du bas arrondis (`radius`) et, en
    /// haut, deux congés creux (`shoulder`) qui la raccordent en douceur au bord de l'écran.
    pub fn notch(&mut self, r: RECT, radius: f32, shoulder: f32, rgb: (u8, u8, u8), alpha: u8) {
        let (l, t, rr, b) = (r.left as f32, r.top as f32, r.right as f32, r.bottom as f32);
        let rad = radius.min((rr - l) / 2.0).min(b - t).max(0.0);
        // Corps : rectangle prolongé vers le haut, pour que seuls les coins du bas soient arrondis.
        let top = t - rad - 1.0;
        let (cx, cy) = ((l + rr) / 2.0, (top + b) / 2.0);
        let (hx, hy) = ((rr - l) / 2.0, (b - top) / 2.0);
        let (x0, x1) = (((l - shoulder).floor() as i32).max(0), ((rr + shoulder).ceil() as i32).min(self.w));
        let (y0, y1) = (r.top.max(0), r.bottom.min(self.h));
        self.touch(x0, y0, x1, y1);
        let full = premul(rgb, alpha);
        // Congés : ce qui reste hors d'un cercle tangent au bord de l'écran et au flanc.
        let sy = t + shoulder;
        let (left_sx, right_sx) = (l - shoulder, rr + shoulder);
        // Hors des coins (et avec un arrondi d'au moins un demi-pixel), le corps est plein sur
        // toute sa largeur, jusqu'au demi-pixel du bord.
        let half = hx - 0.5;
        let (inner_left, inner_right) = (((cx - half - 0.5).ceil() as i32).max(x0), ((cx + half - 0.5).floor() as i32 + 1).min(x1));
        for y in y0..y1 {
            let py = y as f32 + 0.5;
            let qy = (py - cy).abs() - (hy - rad);
            let shoulders = shoulder > 0.0 && py <= sy;
            let row = (y * self.w) as usize;
            let (a, b) = if qy <= 0.0 && rad >= 0.5 && inner_left < inner_right { (inner_left, inner_right) } else { (x1, x1) };
            if a < b {
                blend_span(&mut self.px[row + a as usize..row + b as usize], full);
            }
            for x in (x0..a.min(x1)).chain(b.max(x0)..x1) {
                let px = x as f32 + 0.5;
                let qx = (px - cx).abs() - (hx - rad);
                let sd = outside(qx, qy) + qx.max(qy).min(0.0) - rad;
                let mut cover = (0.5 - sd).clamp(0.0, 1.0);
                if shoulders && cover < 1.0 {
                    if px >= left_sx && px <= l + 0.5 {
                        cover = cover.max(((px - left_sx).hypot(py - sy) - shoulder + 0.5).clamp(0.0, 1.0));
                    }
                    if px <= right_sx && px >= rr - 0.5 {
                        cover = cover.max(((px - right_sx).hypot(py - sy) - shoulder + 0.5).clamp(0.0, 1.0));
                    }
                }
                let i = row + x as usize;
                if cover >= 1.0 {
                    self.px[i] = over(full, self.px[i]);
                } else if cover > 0.0 {
                    self.px[i] = over(premul(rgb, (alpha as f32 * cover).round() as u8), self.px[i]);
                }
            }
        }
    }

    /// Remplit tout ce qui est hors du cercle (`center`, `radius`), bord lissé : posé dans un coin,
    /// ça l'arrondit.
    pub fn fill_outside_circle(&mut self, center: (f32, f32), radius: f32, rgb: (u8, u8, u8), alpha: u8) {
        self.touch(0, 0, self.w, self.h);
        for y in 0..self.h {
            for x in 0..self.w {
                let d = (x as f32 + 0.5 - center.0).hypot(y as f32 + 0.5 - center.1);
                let cover = (d - radius + 0.5).clamp(0.0, 1.0);
                if cover > 0.0 {
                    let i = (y * self.w + x) as usize;
                    self.px[i] = over(premul(rgb, (alpha as f32 * cover).round() as u8), self.px[i]);
                }
            }
        }
    }

    /// Croissant : le disque (`center`, `radius`) privé du disque (`cut`, `cut_radius`), bords
    /// lissés.
    pub fn crescent(&mut self, center: (f32, f32), radius: f32, cut: (f32, f32), cut_radius: f32, rgb: (u8, u8, u8), alpha: u8) {
        let x0 = ((center.0 - radius).floor() as i32).max(0);
        let x1 = ((center.0 + radius).ceil() as i32).min(self.w);
        let y0 = ((center.1 - radius).floor() as i32).max(0);
        let y1 = ((center.1 + radius).ceil() as i32).min(self.h);
        self.touch(x0, y0, x1, y1);
        for y in y0..y1 {
            for x in x0..x1 {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let inside = (radius - (px - center.0).hypot(py - center.1) + 0.5).clamp(0.0, 1.0);
                let outside_cut = ((px - cut.0).hypot(py - cut.1) - cut_radius + 0.5).clamp(0.0, 1.0);
                let cover = inside * outside_cut;
                if cover > 0.0 {
                    let i = (y * self.w + x) as usize;
                    self.px[i] = over(premul(rgb, (alpha as f32 * cover).round() as u8), self.px[i]);
                }
            }
        }
    }

    /// Pose `layer` (de même taille) par-dessus, avec l'opacité `alpha`.
    pub fn overlay(&mut self, layer: &Canvas, alpha: f32) {
        let d = layer.dirty;
        if alpha <= 0.0 || self.w != layer.w || d.left >= d.right || d.top >= d.bottom {
            return;
        }
        self.touch(d.left, d.top, d.right, d.bottom);
        let m = factor(alpha);
        for y in d.top..d.bottom {
            let row = (y * self.w) as usize;
            let span = row + d.left as usize..row + d.right as usize;
            for (dst, &src) in self.px[span.clone()].iter_mut().zip(&layer.px[span]) {
                if src >> 24 != 0 {
                    *dst = over(scale_by(src, m), *dst);
                }
            }
        }
    }

    /// Pose `layer` (de même taille) par-dessus avec l'opacité `alpha`, mais seulement là où le
    /// canevas est déjà peint : le calque est découpé à la forme de ce qu'il recouvre.
    pub fn overlay_inside(&mut self, layer: &Canvas, alpha: f32) {
        let d = layer.dirty;
        if alpha <= 0.0 || self.w != layer.w || d.left >= d.right || d.top >= d.bottom {
            return;
        }
        // Sur un fond opaque (le cas courant), le facteur ne change pas d'un pixel à l'autre.
        let opaque = factor(alpha);
        for y in d.top..d.bottom {
            let row = (y * self.w) as usize;
            let span = row + d.left as usize..row + d.right as usize;
            for (dst, &src) in self.px[span.clone()].iter_mut().zip(&layer.px[span]) {
                if src >> 24 == 0 {
                    continue;
                }
                match *dst >> 24 {
                    0 => {}
                    255 => *dst = over(scale_by(src, opaque), *dst),
                    a => *dst = over(scale(src, alpha * a as f32 / 255.0), *dst),
                }
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
    scale_by(px, factor(k))
}

/// Facteur entier (sur 256) d'une opacité entre 0 et 1.
fn factor(k: f32) -> u32 {
    if k >= 1.0 {
        256
    } else if k <= 0.0 {
        0
    } else {
        (k * 256.0 + 0.5) as u32
    }
}

/// `scale` avec un facteur déjà calculé (voir `factor`).
fn scale_by(px: u32, m: u32) -> u32 {
    match m {
        256 => px,
        0 => 0,
        // Deux canaux à la fois : chacun tient sur 16 bits une fois multiplié (255 × 256).
        _ => {
            let rb = ((px & 0x00FF_00FF) * m + 0x0080_0080) >> 8 & 0x00FF_00FF;
            let ag = (((px >> 8) & 0x00FF_00FF) * m + 0x0080_0080) >> 8 & 0x00FF_00FF;
            rb | ag << 8
        }
    }
}

/// Pose une même couleur (prémultipliée) sur une suite de pixels : le résultat d'`over`, sans
/// division, deux canaux à la fois.
fn blend_span(px: &mut [u32], color: u32) {
    if color >= 0xFF00_0000 {
        px.fill(color);
        return;
    }
    if color == 0 {
        return;
    }
    let keep = 255 - (color >> 24);
    let (src_rb, src_ag) = (color & 0x00FF_00FF, (color >> 8) & 0x00FF_00FF);
    // ⌊x / 255⌋ = (x + 1 + ⌊x / 256⌋) / 256 pour x ≤ 255 × 255, sur chaque moitié de 16 bits.
    let div = |x: u32| ((x + 0x0001_0001 + ((x >> 8) & 0x00FF_00FF)) >> 8) & 0x00FF_00FF;
    for d in px {
        let rb = div((*d & 0x00FF_00FF) * keep);
        let ag = div(((*d >> 8) & 0x00FF_00FF) * keep);
        *d = (src_rb + rb) | (src_ag + ag) << 8;
    }
}

/// Longueur de la partie positive de (`qx`, `qy`) : la distance à un coin arrondi, nulle à
/// l'intérieur (sans racine carrée quand l'un des deux est négatif).
fn outside(qx: f32, qy: f32) -> f32 {
    match (qx > 0.0, qy > 0.0) {
        (true, true) => qx.hypot(qy),
        (true, false) => qx,
        (false, true) => qy,
        (false, false) => 0.0,
    }
}

fn union(a: RECT, b: RECT) -> RECT {
    RECT { left: a.left.min(b.left), top: a.top.min(b.top), right: a.right.max(b.right), bottom: a.bottom.max(b.bottom) }
}

fn intersect(a: RECT, b: RECT) -> RECT {
    RECT { left: a.left.max(b.left), top: a.top.max(b.top), right: a.right.min(b.right), bottom: a.bottom.min(b.bottom) }
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
    // Opaque, il remplace ; vide, il ne change rien (alpha prémultiplié).
    if src >= 0xFF00_0000 {
        return src;
    }
    if src == 0 {
        return dst;
    }
    // Chaque canal : src + ⌊dst × keep / 255⌋, plafonné à 255 ; deux canaux à la fois (voir
    // `blend_span`).
    let keep = 255 - (src >> 24);
    let lanes = |s: u32, d: u32| {
        let x = d * keep;
        let sum = s + (((x + 0x0001_0001 + ((x >> 8) & 0x00FF_00FF)) >> 8) & 0x00FF_00FF);
        (sum | ((sum >> 8) & 0x0001_0001) * 0xFF) & 0x00FF_00FF
    };
    lanes(src & 0x00FF_00FF, dst & 0x00FF_00FF) | lanes((src >> 8) & 0x00FF_00FF, (dst >> 8) & 0x00FF_00FF) << 8
}

/// Bitmap 32 bits sélectionné dans un DC mémoire, dont on peut lire et écrire les pixels.
pub(crate) struct Dib {
    pub dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u32,
    pub w: i32,
    pub h: i32,
}

impl Dib {
    pub unsafe fn new(w: i32, h: i32) -> Option<Self> {
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
        let bmp = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
        let dc = CreateCompatibleDC(None);
        let old = SelectObject(dc, bmp);
        Some(Self { dc, bmp, old, bits: bits as *mut u32, w, h })
    }

    #[allow(clippy::mut_from_ref)]
    pub fn pixels(&self) -> &mut [u32] {
        unsafe { std::slice::from_raw_parts_mut(self.bits, (self.w * self.h) as usize) }
    }
}

/// Les deux bitmaps de `Canvas::gdi`, l'un noir, l'autre blanc : ils le redeviennent après
/// chaque usage.
struct GdiPair {
    black: Dib,
    white: Dib,
    w: i32,
    h: i32,
}

impl GdiPair {
    fn new(w: i32, h: i32) -> Option<Self> {
        let (black, white) = unsafe { (Dib::new(w, h)?, Dib::new(w, h)?) };
        black.pixels().fill(0x0000_0000);
        white.pixels().fill(0x00FF_FFFF);
        Some(Self { black, white, w, h })
    }
}

thread_local! {
    static GDI_PAIR: RefCell<Option<GdiPair>> = const { RefCell::new(None) };
    /// Bitmap remis à UpdateLayeredWindow (voir `Canvas::present`).
    static SURFACE: RefCell<Option<Dib>> = const { RefCell::new(None) };
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
    fn une_image_reduite_est_moyennee() {
        // Une colonne blanche sur huit, réduite huit fois : un gris sombre partout, alors qu'un
        // seul échantillon par pixel tomberait toujours entre deux colonnes et ne verrait que du noir.
        let px = (0..64 * 64).map(|i| if i % 64 % 8 == 0 { 0xFFFF_FFFF } else { 0xFF00_0000 }).collect();
        let img = Image { w: 64, h: 64, px };
        let mut c = Canvas::new(8, 8);
        c.draw_quad(&img, [(0.0, 0.0), (8.0, 0.0), (8.0, 8.0), (0.0, 8.0)], 255, 0);
        for y in 2..6 {
            for x in 2..6 {
                let red = channel(c.px[(y * 8 + x) as usize], 16);
                assert!((20..=45).contains(&red), "gris sombre attendu en ({x}, {y}), reçu {red}");
            }
        }
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
    fn panneau_plein_avec_liseré() {
        let backdrop = Image { w: 40, h: 40, px: vec![premul((0, 100, 0), 255); 1600] };
        let mut c = Canvas::new(40, 40);
        let look = Material { tint: (30, 30, 34), opacity: 0.8, border: 0.12 };
        c.material(RECT { left: 5, top: 5, right: 35, bottom: 35 }, 8.0, Some(&backdrop), (0, 0), &look);
        let center = c.px[(20 * 40 + 20) as usize];
        assert_eq!(center >> 24, 255, "le centre est couvert");
        assert_eq!(c.px[(5 * 40 + 5) as usize] >> 24, 0, "le coin arrondi reste vide");
        // Surtout la teinte (30) ; le vert du fond (100) ne transparaît qu'un peu.
        assert!((40..=50).contains(&channel(center, 8)), "vert : {}", channel(center, 8));
        // Le liseré, sur le bord du haut, est plus clair que l'intérieur.
        assert!(channel(c.px[(5 * 40 + 20) as usize], 16) > channel(center, 16));
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
    fn rectangle_sans_arrondi_entierement_couvert() {
        // Zone « invisible mais pleine » : alpha 1 partout, jamais 0 (sinon la souris la traverse).
        let mut c = Canvas::new(10, 10);
        c.round_rect(RECT { left: 0, top: 0, right: 10, bottom: 10 }, 0, (0, 0, 0), 1);
        assert!(c.px.iter().all(|p| p >> 24 == 1));
        let mut c = Canvas::new(10, 10);
        c.round_rect(RECT { left: 0, top: 0, right: 10, bottom: 10 }, 0, (255, 255, 255), 90);
        assert_eq!(c.px[55] >> 24, 90);
    }

    #[test]
    fn encoche_avec_congés() {
        let mut c = Canvas::new(40, 20);
        c.notch(RECT { left: 10, top: 0, right: 30, bottom: 16 }, 6.0, 4.0, (0, 0, 0), 255);
        let alpha = |x: i32, y: i32| c.px[(y * 40 + x) as usize] >> 24;
        assert_eq!(alpha(20, 8), 255, "le corps est plein");
        assert_eq!(alpha(10, 0), 255, "le coin du haut reste carré");
        assert_eq!(alpha(10, 15), 0, "le coin du bas est arrondi");
        assert_eq!(alpha(9, 0), 255, "le congé prolonge le haut vers la gauche");
        assert_eq!(alpha(30, 0), 255, "et vers la droite");
        assert_eq!(alpha(7, 3), 0, "il se creuse en descendant");
        assert_eq!(alpha(9, 8), 0, "rien sous le congé");
    }

    #[test]
    fn coin_d_ecran_arrondi() {
        // Coin en haut à gauche : le cercle est centré sur le coin opposé du carré.
        let mut c = Canvas::new(12, 12);
        c.fill_outside_circle((12.0, 12.0), 12.0, (0, 0, 0), 255);
        let alpha = |x: i32, y: i32| c.px[(y * 12 + x) as usize] >> 24;
        assert_eq!(alpha(0, 0), 255, "le coin est noir");
        assert_eq!(alpha(11, 11), 0, "le reste de l'écran reste visible");
        assert!(alpha(11, 0) < 20, "au bout de la courbe, presque rien");
        let edge = (0..12).map(|i| alpha(i, i)).filter(|a| *a > 0 && *a < 255).count();
        assert!(edge > 0, "la courbe est lissée");
    }

    /// L'ancienne version de `Canvas::gdi` : deux bitmaps neufs de la taille du canevas, relus en
    /// entier.
    fn gdi_reference(c: &mut Canvas, draw: impl Fn(HDC)) {
        unsafe {
            let (black, white) = (Dib::new(c.w, c.h).unwrap(), Dib::new(c.w, c.h).unwrap());
            black.pixels().fill(0x0000_0000);
            white.pixels().fill(0x00FF_FFFF);
            draw(black.dc);
            draw(white.dc);
            let _ = GdiFlush();
            for ((dst, &b), &w) in c.px.iter_mut().zip(black.pixels().iter()).zip(white.pixels().iter()) {
                let diff = (channel(w, 16) + channel(w, 8) + channel(w, 0)) - (channel(b, 16) + channel(b, 8) + channel(b, 0));
                let alpha = 255 - (diff / 3).clamp(0, 255);
                if alpha > 0 {
                    let c = |shift| channel(b, shift).min(alpha) as u32;
                    *dst = over((alpha as u32) << 24 | c(16) << 16 | c(8) << 8 | c(0), *dst);
                }
            }
        }
    }

    #[test]
    fn gdi_ne_relit_que_la_zone_dessinee_sans_rien_perdre() {
        use windows::Win32::UI::WindowsAndMessaging::*;
        let text = |at: i32, words: &'static str| {
            move |dc: HDC| unsafe {
                SetBkMode(dc, TRANSPARENT);
                SetTextColor(dc, COLORREF(0x00F0_F0F0));
                let mut wide: Vec<u16> = words.encode_utf16().collect();
                let mut r = RECT { left: at, top: 4, right: at + 400, bottom: 40 };
                DrawTextW(dc, &mut wide, &mut r, DT_LEFT | DT_SINGLELINE | DT_NOPREFIX);
            }
        };
        let icon = |dc: HDC| unsafe {
            let icon = LoadIconW(None, IDI_APPLICATION).unwrap();
            let _ = DrawIconEx(dc, 30, 20, icon, 32, 32, 0, None, DI_NORMAL);
        };
        // Plusieurs passages de suite, de tailles différentes : les bitmaps gardés doivent
        // redevenir propres à chaque fois, et ce qui déborde du canevas rester coupé.
        let cases: [(i32, i32, &dyn Fn(HDC)); 4] = [
            (300, 60, &text(10, "Titre — 3:07 Égaliseur")),
            (120, 40, &text(60, "Déborde largement du canevas")),
            (300, 80, &icon),
            (200, 60, &text(0, "Encore")),
        ];
        for (w, h, draw) in cases {
            let mut fast = Canvas::new(w, h);
            fast.round_rect(RECT { left: 0, top: 0, right: w, bottom: h }, 6, (20, 20, 24), 200);
            let mut slow = Canvas::new(w, h);
            slow.round_rect(RECT { left: 0, top: 0, right: w, bottom: h }, 6, (20, 20, 24), 200);
            fast.gdi(draw);
            gdi_reference(&mut slow, draw);
            assert!(fast.px == slow.px, "{w}×{h} : le dessin GDI diffère");
        }
    }

    #[test]
    fn polygone_identique_avec_ou_sans_raccourci() {
        // Référence : les 16 échantillons comptés pour chaque pixel.
        fn reference(c: &mut Canvas, points: &[(f32, f32)], rgb: (u8, u8, u8), alpha: u8) {
            let n = points.len();
            let sign = (0..n).map(|i| points[i].0 * points[(i + 1) % n].1 - points[(i + 1) % n].0 * points[i].1).sum::<f32>().signum();
            let inside = |x: f32, y: f32| {
                (0..n).all(|i| {
                    let (a, b) = (points[i], points[(i + 1) % n]);
                    ((b.0 - a.0) * (y - a.1) - (b.1 - a.1) * (x - a.0)) * sign >= 0.0
                })
            };
            for y in 0..c.h {
                for x in 0..c.w {
                    let hits = (0..16).filter(|k| inside(x as f32 + (k % 4) as f32 / 4.0 + 0.125, y as f32 + (k / 4) as f32 / 4.0 + 0.125)).count();
                    if hits > 0 {
                        let i = (y * c.w + x) as usize;
                        c.px[i] = over(premul(rgb, (alpha as usize * hits / 16) as u8), c.px[i]);
                    }
                }
            }
        }
        let shapes: [Vec<(f32, f32)>; 4] = [
            round_corners(&[(4.0, 2.0), (38.0, 20.0), (4.0, 37.0)], 2.2),
            vec![(3.3, 5.1), (30.7, 2.9), (36.2, 33.4), (6.6, 30.8)],
            round_corners(&[(10.0, 10.0), (12.0, 10.0), (11.0, 30.0)], 3.0),
            vec![(20.0, 1.5), (38.5, 20.0), (20.0, 38.5), (1.5, 20.0)],
        ];
        for shape in shapes {
            let (mut fast, mut slow) = (Canvas::new(40, 40), Canvas::new(40, 40));
            fast.fill_convex(&shape, (255, 200, 100), 230);
            reference(&mut slow, &shape, (255, 200, 100), 230);
            assert!(fast.px == slow.px, "{shape:?}");
        }
    }

    #[test]
    fn opacite_appliquee_a_un_pixel_pres() {
        let old = |px: u32, k: f32| {
            let mul = |shift: u32| ((((px >> shift) & 0xFF) as f32 * k).round() as u32).min(255) << shift;
            mul(24) | mul(16) | mul(8) | mul(0)
        };
        for px in [0xFFFF_FFFF, 0x80_40_20_10, 0x7F_7F_00_7F, 0x01_01_01_00, 0xFF_00_80_FF] {
            for k in [0.0, 0.003, 0.1, 0.25, 0.5, 0.7, 0.99, 1.0] {
                let (a, b) = (scale(px, k), old(px, k));
                for shift in [0, 8, 16, 24] {
                    assert!((channel(a, shift) - channel(b, shift)).abs() <= 1, "{px:08x} × {k}");
                }
            }
        }
    }

    #[test]
    fn calques_fondus_seulement_sur_leur_zone() {
        let mut base = Canvas::new(30, 20);
        base.round_rect(RECT { left: 0, top: 0, right: 30, bottom: 20 }, 0, (10, 10, 10), 255);
        let mut layer = Canvas::new(30, 20);
        layer.round_rect(RECT { left: 5, top: 5, right: 12, bottom: 9 }, 2, (255, 255, 255), 255);
        let mut full = Canvas::new(30, 20);
        full.px.copy_from_slice(&base.px);
        base.overlay_inside(&layer, 0.6);
        for (dst, &src) in full.px.iter_mut().zip(&layer.px) {
            if src >> 24 != 0 {
                *dst = over(scale(src, 0.6 * (*dst >> 24) as f32 / 255.0), *dst);
            }
        }
        assert!(base.px == full.px);
    }

    #[test]
    fn superposition_identique_a_la_formule_canal_par_canal() {
        let reference = |src: u32, dst: u32| {
            let keep = 255 - (src >> 24);
            let mix = |shift: u32| (((src >> shift) & 0xFF) + ((dst >> shift) & 0xFF) * keep / 255).min(255) << shift;
            mix(24) | mix(16) | mix(8) | mix(0)
        };
        let mut seed = 987u32;
        let mut next = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            seed
        };
        for _ in 0..200_000 {
            // Y compris des pixels mal prémultipliés, pour le plafond à 255.
            let (src, dst) = (next(), next());
            assert_eq!(over(src, dst), reference(src, dst), "{src:08x} sur {dst:08x}");
        }
    }

    #[test]
    fn remplissage_d_une_ligne_identique_a_over() {
        for x in 0..=255 * 255u32 {
            assert_eq!((x + 1 + (x >> 8)) >> 8, x / 255, "{x}");
        }
        let colors = [premul((10, 132, 255), 90), premul((0, 0, 0), 35), premul((255, 255, 255), 1), premul((28, 28, 32), 230), premul((1, 2, 3), 255)];
        let mut seed = 12345u32;
        let mut dst: Vec<u32> = (0..4000)
            .map(|_| {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                let a = seed >> 24;
                let c = |shift: u32| ((seed >> shift) & 0xFF).min(a);
                a << 24 | c(16) << 16 | c(8) << 8 | c(0)
            })
            .collect();
        dst.extend([0, 0xFF00_0000, 0xFFFF_FFFF, 0x8080_8080]);
        for color in colors {
            let mut fast = dst.clone();
            blend_span(&mut fast, color);
            let slow: Vec<u32> = dst.iter().map(|&d| over(color, d)).collect();
            assert!(fast == slow, "{color:08x}");
        }
    }

    #[test]
    fn rectangles_et_encoche_identiques_au_pixel_pres() {
        // Les versions d'avant, pixel par pixel.
        fn rect_reference(c: &mut Canvas, r: RECT, radius: i32, rgb: (u8, u8, u8), alpha: u8) {
            let rad = radius.min((r.right - r.left) / 2).min((r.bottom - r.top) / 2).max(0) as f32;
            let (left, top, right, bottom) = (r.left as f32 + rad, r.top as f32 + rad, r.right as f32 - rad, r.bottom as f32 - rad);
            for y in r.top.max(0)..r.bottom.min(c.h) {
                for x in r.left.max(0)..r.right.min(c.w) {
                    let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                    let d = (px - px.clamp(left, right)).hypot(py - py.clamp(top, bottom));
                    let cover = if d <= 0.0 { 1.0 } else { (rad - d + 0.5).clamp(0.0, 1.0) };
                    if cover > 0.0 {
                        let i = (y * c.w + x) as usize;
                        c.px[i] = over(premul(rgb, (alpha as f32 * cover).round() as u8), c.px[i]);
                    }
                }
            }
        }
        fn notch_reference(c: &mut Canvas, r: RECT, radius: f32, shoulder: f32, rgb: (u8, u8, u8), alpha: u8) {
            let (l, t, rr, b) = (r.left as f32, r.top as f32, r.right as f32, r.bottom as f32);
            let rad = radius.min((rr - l) / 2.0).min(b - t).max(0.0);
            let top = t - rad - 1.0;
            let (cx, cy) = ((l + rr) / 2.0, (top + b) / 2.0);
            let (hx, hy) = ((rr - l) / 2.0, (b - top) / 2.0);
            for y in r.top.max(0)..r.bottom.min(c.h) {
                for x in ((l - shoulder).floor() as i32).max(0)..((rr + shoulder).ceil() as i32).min(c.w) {
                    let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                    let (qx, qy) = ((px - cx).abs() - (hx - rad), (py - cy).abs() - (hy - rad));
                    let sd = qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - rad;
                    let mut cover = (0.5 - sd).clamp(0.0, 1.0);
                    let sy = t + shoulder;
                    for sx in [l - shoulder, rr + shoulder] {
                        let beside = if sx < l { px >= sx && px <= l + 0.5 } else { px <= sx && px >= rr - 0.5 };
                        if shoulder > 0.0 && beside && py <= sy {
                            cover = cover.max(((px - sx).hypot(py - sy) - shoulder + 0.5).clamp(0.0, 1.0));
                        }
                    }
                    if cover > 0.0 {
                        let i = (y * c.w + x) as usize;
                        c.px[i] = over(premul(rgb, (alpha as f32 * cover).round() as u8), c.px[i]);
                    }
                }
            }
        }
        let rects = [
            (RECT { left: -5, top: -3, right: 70, bottom: 50 }, 12, 35),
            (RECT { left: 3, top: 4, right: 9, bottom: 30 }, 3, 255),
            (RECT { left: 10, top: 10, right: 60, bottom: 12 }, 1, 200),
            (RECT { left: 0, top: 0, right: 64, bottom: 48 }, 0, 90),
            (RECT { left: 20, top: 5, right: 40, bottom: 45 }, 40, 255),
        ];
        let (mut fast, mut slow) = (Canvas::new(64, 48), Canvas::new(64, 48));
        for (r, radius, alpha) in rects {
            fast.round_rect(r, radius, (200, 100, 50), alpha);
            rect_reference(&mut slow, r, radius, (200, 100, 50), alpha);
        }
        assert!(fast.px == slow.px, "rectangles");
        let notches = [
            (RECT { left: 10, top: 0, right: 54, bottom: 30 }, 10.0, 6.0, 255),
            (RECT { left: 15, top: 0, right: 49, bottom: 20 }, 7.3, 4.5, 120),
            (RECT { left: 30, top: 0, right: 34, bottom: 40 }, 0.3, 0.0, 255),
            (RECT { left: -8, top: 0, right: 70, bottom: 47 }, 22.0, 9.0, 255),
        ];
        for (r, radius, shoulder, alpha) in notches {
            let (mut fast, mut slow) = (Canvas::new(64, 48), Canvas::new(64, 48));
            fast.round_rect(RECT { left: 0, top: 0, right: 64, bottom: 48 }, 0, (40, 90, 30), 180);
            slow.round_rect(RECT { left: 0, top: 0, right: 64, bottom: 48 }, 0, (40, 90, 30), 180);
            fast.notch(r, radius, shoulder, (0, 0, 0), alpha);
            notch_reference(&mut slow, r, radius, shoulder, (0, 0, 0), alpha);
            assert!(fast.px == slow.px, "encoche {radius} {shoulder}");
        }
    }

    #[test]
    fn coins_arrondis_lisses() {
        let mut c = Canvas::new(20, 20);
        c.round_rect(RECT { left: 0, top: 0, right: 20, bottom: 20 }, 8, (255, 255, 255), 255);
        assert_eq!(c.px[0] >> 24, 0, "le coin est vide");
        assert_eq!(c.px[(10 * 20 + 10) as usize] >> 24, 255, "le centre est plein");
    }
}
