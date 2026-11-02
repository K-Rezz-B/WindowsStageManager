//! Outils du shell Windows pour le Dock : raccourcis épinglés, grandes icônes, noms d'applis,
//! lancement et corbeille. Les fonctions qui utilisent COM s'appellent depuis un fil où COM est
//! initialisé (voir `IconLoader`).

use std::{
    ffi::c_void,
    mem::size_of,
    path::Path,
    sync::{mpsc, Arc, Mutex},
    thread,
};

use windows::{
    core::*,
    Win32::{
        Foundation::*,
        Graphics::Gdi::*,
        Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW},
        System::Com::*,
        UI::{
            Shell::*,
            WindowsAndMessaging::{PostMessageW, SW_SHOWNORMAL},
        },
    },
};

use crate::canvas::Image;

/// Corbeille, désignée par son identifiant de dossier shell.
pub const RECYCLE_BIN: &str = "::{645FF040-5081-101B-9F08-00AA002F954E}";

/// Raccourcis épinglés dans la barre des tâches de Windows (pour garnir le Dock au départ).
pub fn taskbar_pins() -> Vec<String> {
    let Ok(appdata) = std::env::var("APPDATA") else { return Vec::new() };
    let dir = format!("{appdata}\\Microsoft\\Internet Explorer\\Quick Launch\\User Pinned\\TaskBar");
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut pins: Vec<String> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("lnk")))
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    pins.sort();
    pins
}

/// Toutes les applis installées (y compris celles du Microsoft Store), telles que les liste le
/// dossier « Applications » de Windows : (nom, chemin à ouvrir). À appeler depuis un fil COM.
pub fn installed_apps() -> Vec<(String, String)> {
    let mut apps = Vec::new();
    unsafe {
        let folder: Result<IShellItem> = SHGetKnownFolderItem(&FOLDERID_AppsFolder, KF_FLAG_DEFAULT, HANDLE::default());
        let Ok(folder) = folder else { return apps };
        let Ok(items) = folder.BindToHandler::<_, IEnumShellItems>(None, &BHID_EnumItems) else { return apps };
        loop {
            let mut batch: [Option<IShellItem>; 1] = [None];
            if items.Next(&mut batch, None).is_err() {
                break;
            }
            let Some(item) = batch[0].take() else { break };
            let text = |kind| -> Option<String> {
                let raw = item.GetDisplayName(kind).ok()?;
                let text = raw.to_string().ok();
                CoTaskMemFree(Some(raw.0 as *const c_void));
                text
            };
            if let (Some(name), Some(id)) = (text(SIGDN_NORMALDISPLAY), text(SIGDN_PARENTRELATIVEPARSING)) {
                apps.push((name, format!("shell:AppsFolder\\{id}")));
            }
        }
    }
    apps
}

/// Ce qu'on sait d'un élément du Dock une fois chargé.
pub struct Loaded {
    pub path: String,
    pub name: String,
    /// Exécutable visé, en minuscules (vide si inconnu, par exemple pour une appli du Store).
    pub target: String,
    pub icon: Option<Image>,
}

/// Charge icônes et noms dans un fil à part (l'extraction d'icônes peut prendre du temps).
pub struct IconLoader {
    requests: mpsc::Sender<String>,
    done: Arc<Mutex<Vec<Loaded>>>,
}

impl IconLoader {
    /// `message` est posté à `notify` chaque fois qu'un élément est prêt.
    pub fn start(notify: HWND, message: u32) -> Self {
        let (requests, rx) = mpsc::channel::<String>();
        let done = Arc::new(Mutex::new(Vec::new()));
        let shared = Arc::clone(&done);
        let notify = notify.0 as isize;
        thread::spawn(move || {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            }
            for path in rx {
                let loaded = load(&path);
                if let Ok(mut done) = shared.lock() {
                    done.push(loaded);
                }
                unsafe {
                    let _ = PostMessageW(HWND(notify as _), message, WPARAM(0), LPARAM(0));
                }
            }
        });
        Self { requests, done }
    }

    pub fn request(&self, path: &str) {
        let _ = self.requests.send(path.to_owned());
    }

    pub fn take(&self) -> Vec<Loaded> {
        self.done.lock().map(|mut d| std::mem::take(&mut *d)).unwrap_or_default()
    }
}

fn load(path: &str) -> Loaded {
    let is_link = path.to_lowercase().ends_with(".lnk");
    let target = if is_link { link_target(path).unwrap_or_default() } else { path.to_owned() }.to_lowercase();
    let stem = |p: &str| Path::new(p).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let name = if path == RECYCLE_BIN {
        "Corbeille".to_owned()
    } else if is_link {
        stem(path)
    } else {
        file_description(path).unwrap_or_else(|| stem(path))
    };
    Loaded { path: path.to_owned(), name, target, icon: big_icon(path) }
}

/// Cible d'un raccourci .lnk.
fn link_target(path: &str) -> Option<String> {
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        let file: IPersistFile = link.cast().ok()?;
        file.Load(&HSTRING::from(path), STGM_READ).ok()?;
        let mut buf = [0u16; 1024];
        link.GetPath(&mut buf, std::ptr::null_mut(), 0).ok()?;
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        (len > 0).then(|| String::from_utf16_lossy(&buf[..len]))
    }
}

/// Nom lisible d'un exécutable (« Description du fichier » dans ses propriétés).
fn file_description(path: &str) -> Option<String> {
    unsafe {
        let file = HSTRING::from(path);
        let size = GetFileVersionInfoSizeW(&file, None);
        if size == 0 {
            return None;
        }
        let mut data = vec![0u8; size as usize];
        GetFileVersionInfoW(&file, 0, size, data.as_mut_ptr().cast()).ok()?;
        let mut ptr: *mut c_void = std::ptr::null_mut();
        let mut len = 0u32;
        if !VerQueryValueW(data.as_ptr().cast(), w!("\\VarFileInfo\\Translation"), &mut ptr, &mut len).as_bool() || len < 4 {
            return None;
        }
        let (lang, codepage) = (*(ptr as *const u16), *(ptr as *const u16).add(1));
        let key = HSTRING::from(format!("\\StringFileInfo\\{lang:04x}{codepage:04x}\\FileDescription"));
        if !VerQueryValueW(data.as_ptr().cast(), &key, &mut ptr, &mut len).as_bool() || len == 0 {
            return None;
        }
        let text = String::from_utf16_lossy(std::slice::from_raw_parts(ptr as *const u16, len as usize));
        let text = text.trim_end_matches('\0').trim().to_owned();
        (!text.is_empty()).then_some(text)
    }
}

/// Grande icône (256 px demandés, réduite à 128) d'un fichier, d'un raccourci ou d'un dossier shell.
pub fn big_icon(path: &str) -> Option<Image> {
    unsafe {
        let factory: IShellItemImageFactory = SHCreateItemFromParsingName(&HSTRING::from(path), None).ok()?;
        let bmp = factory.GetImage(SIZE { cx: 256, cy: 256 }, SIIGBF_ICONONLY | SIIGBF_BIGGERSIZEOK).ok()?;
        let image = bitmap_pixels(bmp);
        let _ = DeleteObject(bmp);
        let mut image = crop_to_content(image?);
        while image.w > 128 {
            image = image.half();
        }
        Some(image)
    }
}

/// Une appli qui n'a qu'une petite icône la reçoit au milieu d'une grande image vide :
/// on recadre sur le dessin pour qu'il remplisse sa place dans le Dock.
fn crop_to_content(image: Image) -> Image {
    let (w, h) = (image.w, image.h);
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, -1, -1);
    for y in 0..h {
        for x in 0..w {
            if image.px[(y * w + x) as usize] >> 24 > 16 {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
        }
    }
    let side = (x1 - x0 + 1).max(y1 - y0 + 1);
    if x1 < 0 || side * 10 >= w * 7 {
        return image;
    }
    // Carré centré sur le dessin, avec une petite marge.
    let side = (side * 108 / 100).min(w).min(h);
    let cx = (x0 + x1) / 2;
    let cy = (y0 + y1) / 2;
    let left = (cx - side / 2).clamp(0, w - side);
    let top = (cy - side / 2).clamp(0, h - side);
    let mut px = Vec::with_capacity((side * side) as usize);
    for y in top..top + side {
        px.extend_from_slice(&image.px[(y * w + left) as usize..(y * w + left + side) as usize]);
    }
    Image { w: side, h: side, px }
}

unsafe fn bitmap_pixels(bmp: HBITMAP) -> Option<Image> {
    let mut bm = BITMAP::default();
    if GetObjectW(bmp, size_of::<BITMAP>() as i32, Some(&mut bm as *mut BITMAP as *mut c_void)) == 0 {
        return None;
    }
    let (w, h) = (bm.bmWidth, bm.bmHeight.abs());
    if w <= 0 || h <= 0 {
        return None;
    }
    let mut info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut px = vec![0u32; (w * h) as usize];
    let screen = GetDC(None);
    let lines = GetDIBits(screen, bmp, 0, h as u32, Some(px.as_mut_ptr().cast()), &mut info, DIB_RGB_COLORS);
    ReleaseDC(None, screen);
    if lines == 0 {
        return None;
    }
    let ch = |p: u32, s: u32| (p >> s) & 0xFF;
    if px.iter().all(|&p| p >> 24 == 0) {
        // Pas de canal alpha (petites icônes anciennes, posées au milieu d'une image noire) :
        // le noir pur est le fond, le reste est opaque.
        px.iter_mut().for_each(|p| *p = if *p & 0x00FF_FFFF == 0 { 0 } else { *p | 0xFF00_0000 });
    } else if px.iter().any(|&p| ch(p, 16) > ch(p, 24) || ch(p, 8) > ch(p, 24) || ch(p, 0) > ch(p, 24)) {
        // Alpha non prémultiplié : on le prémultiplie.
        for p in px.iter_mut() {
            let a = ch(*p, 24);
            let m = |s: u32| ch(*p, s) * a / 255;
            *p = a << 24 | m(16) << 16 | m(8) << 8 | m(0);
        }
    }
    Some(Image { w, h, px })
}

/// Lance un raccourci, un exécutable ou ouvre un dossier shell.
pub fn open(path: &str) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        ShellExecuteW(None, w!("open"), &HSTRING::from(path), None, None, SW_SHOWNORMAL);
    }
}

pub fn open_recycle_bin() {
    open("shell:RecycleBinFolder");
}

/// Vide la corbeille ; Windows demande lui-même confirmation.
pub fn empty_recycle_bin(owner: HWND) {
    unsafe {
        let _ = SHEmptyRecycleBinW(owner, None, 0);
    }
}

pub fn recycle_bin_items() -> i64 {
    let mut info = SHQUERYRBINFO { cbSize: size_of::<SHQUERYRBINFO>() as u32, ..Default::default() };
    unsafe { SHQueryRecycleBinW(None, &mut info) }.map(|_| info.i64NumItems).unwrap_or(0)
}
