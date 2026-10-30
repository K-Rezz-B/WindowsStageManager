//! Le fond d'écran derrière la barre, flouté : c'est ce qu'on voit à travers le verre du lecteur.
//!
//! On ne peut pas lire ce qu'il y a derrière une fenêtre, mais la barre est une zone réservée
//! (aucune fenêtre agrandie n'y va) : derrière elle, il n'y a que le fond d'écran. On le relit
//! depuis le fichier que Windows affiche, en reproduisant son ajustement à l'écran.

use std::{
    sync::{Arc, Mutex},
    thread,
};

use windows::{
    core::*,
    Graphics::Imaging::*,
    Storage::{Streams::IRandomAccessStream, StorageFile},
    Win32::{
        Foundation::*,
        System::{
            Registry::*,
            WinRT::{RoInitialize, RO_INIT_MULTITHREADED},
        },
        UI::WindowsAndMessaging::PostMessageW,
    },
};

use crate::canvas::Image;

/// Charge le fond d'écran derrière `bar` (coordonnées écran) dans un fil à part, le floute,
/// le dépose dans `out` puis poste `message` à `notify`.
pub fn load_async(bar: RECT, monitor: RECT, blur: i32, notify: HWND, message: u32, out: Arc<Mutex<Option<Image>>>) {
    let notify = notify.0 as isize;
    thread::spawn(move || {
        unsafe {
            let _ = RoInitialize(RO_INIT_MULTITHREADED);
        }
        let image = load(bar, monitor).map(|mut img| {
            img.blur(blur);
            img
        });
        if let Ok(mut shared) = out.lock() {
            *shared = image;
        }
        unsafe {
            let _ = PostMessageW(HWND(notify as _), message, WPARAM(0), LPARAM(0));
        }
    });
}

fn load(bar: RECT, monitor: RECT) -> Option<Image> {
    // Copie de travail du fond d'écran actuel, tenue à jour par Windows (diaporama compris).
    let path = format!("{}\\Microsoft\\Windows\\Themes\\TranscodedWallpaper", std::env::var("APPDATA").ok()?);
    let file = StorageFile::GetFileFromPathAsync(&HSTRING::from(path)).ok()?.get().ok()?;
    let stream: IRandomAccessStream = file.OpenReadAsync().ok()?.get().ok()?.cast().ok()?;
    let decoder = BitmapDecoder::CreateAsync(&stream).ok()?.get().ok()?;
    let (iw, ih) = (decoder.OrientedPixelWidth().ok()? as f64, decoder.OrientedPixelHeight().ok()? as f64);
    let (mw, mh) = ((monitor.right - monitor.left) as f64, (monitor.bottom - monitor.top) as f64);
    if iw <= 0.0 || ih <= 0.0 || mw <= 0.0 || mh <= 0.0 {
        return None;
    }

    // Taille de l'image une fois ajustée à l'écran, comme le fait Windows.
    let (sw, sh) = match wallpaper_style().as_deref() {
        Some("2") => (mw, mh),                                  // Étirer
        Some("6") => scaled(iw, ih, (mw / iw).min(mh / ih)),     // Ajuster
        Some("0") => (iw, ih),                                  // Centrer
        _ => scaled(iw, ih, (mw / iw).max(mh / ih)),            // Remplir (par défaut)
    };
    let (ox, oy) = ((mw - sw) / 2.0, (mh - sh) / 2.0);

    // Partie de l'image ajustée qui se trouve derrière la barre.
    let (bw, bh) = (bar.right - bar.left, bar.bottom - bar.top);
    let (bx, by) = ((bar.left - monitor.left) as f64 - ox, (bar.top - monitor.top) as f64 - oy);
    let x0 = bx.max(0.0).min(sw) as u32;
    let y0 = by.max(0.0).min(sh) as u32;
    let x1 = (bx + bw as f64).max(0.0).min(sw) as u32;
    let y1 = (by + bh as f64).max(0.0).min(sh) as u32;
    let mut image = Image { w: bw, h: bh, px: vec![0xFF00_0000; (bw.max(0) * bh.max(0)) as usize] };
    if x1 <= x0 || y1 <= y0 {
        return Some(image);
    }

    let transform = BitmapTransform::new().ok()?;
    transform.SetScaledWidth(sw.round() as u32).ok()?;
    transform.SetScaledHeight(sh.round() as u32).ok()?;
    transform.SetInterpolationMode(BitmapInterpolationMode::Fant).ok()?;
    transform.SetBounds(BitmapBounds { X: x0, Y: y0, Width: x1 - x0, Height: y1 - y0 }).ok()?;
    let data = decoder
        .GetPixelDataTransformedAsync(
            BitmapPixelFormat::Bgra8,
            BitmapAlphaMode::Ignore,
            &transform,
            ExifOrientationMode::RespectExifOrientation,
            ColorManagementMode::DoNotColorManage,
        )
        .ok()?
        .get()
        .ok()?;
    let bytes = data.DetachPixelData().ok()?;
    let (rw, rh) = ((x1 - x0) as usize, (y1 - y0) as usize);
    if bytes.len() < rw * rh * 4 {
        return None;
    }
    // Recopie à sa place dans la barre (si l'image ne la couvre pas entièrement, le reste est noir).
    let (dx, dy) = ((x0 as f64 - bx).round() as i32, (y0 as f64 - by).round() as i32);
    for row in 0..rh {
        let y = dy + row as i32;
        if y < 0 || y >= bh {
            continue;
        }
        for col in 0..rw {
            let x = dx + col as i32;
            if x < 0 || x >= bw {
                continue;
            }
            let b = &bytes[(row * rw + col) * 4..][..4];
            image.px[(y * bw + x) as usize] = u32::from_le_bytes([b[0], b[1], b[2], 0xFF]);
        }
    }
    Some(image)
}

fn scaled(w: f64, h: f64, k: f64) -> (f64, f64) {
    (w * k, h * k)
}

/// Mode d'ajustement du fond d'écran (« WallpaperStyle » : 10 remplir, 6 ajuster, 2 étirer…).
fn wallpaper_style() -> Option<String> {
    let mut buf = [0u16; 16];
    let mut size = (buf.len() * 2) as u32;
    let err = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Control Panel\\Desktop"),
            w!("WallpaperStyle"),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size as *mut u32),
        )
    };
    (err == ERROR_SUCCESS).then(|| String::from_utf16_lossy(&buf[..(size as usize / 2).saturating_sub(1)]))
}
