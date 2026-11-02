//! Petites fonctions utilitaires autour de l'API Win32.

use std::{ffi::c_void, mem::size_of};

use windows::{
    core::*,
    Win32::{
        Foundation::*,
        Graphics::{
            Dwm::*,
            Gdi::{GetMonitorInfoW, MonitorFromWindow, HMONITOR, MONITORINFO, MONITOR_DEFAULTTONEAREST},
        },
        Security::*,
        System::{LibraryLoader::GetModuleHandleW, Threading::*},
        UI::{
            HiDpi::{GetDpiForSystem, GetSystemMetricsForDpi},
            Input::KeyboardAndMouse::*,
            WindowsAndMessaging::*,
        },
    },
};

use crate::stages::Wid;

pub fn hwnd_of(w: Wid) -> HWND {
    HWND(w as *mut c_void)
}

pub fn wid_of(h: HWND) -> Wid {
    h.0 as Wid
}

/// Fenêtres système ou du shell qu'on ne doit jamais gérer.
const IGNORED_CLASSES: &[&str] = &[
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
    "Progman",
    "WorkerW",
    "Windows.UI.Core.CoreWindow",
    "XamlExplorerHostIslandWindow",
    "ForegroundStaging",
    "MultitaskingViewFrame",
    "TopLevelWindowForOverflowXamlIsland",
    "NotifyIconOverflowWindow",
];

pub fn top_level_windows() -> Vec<HWND> {
    unsafe extern "system" fn collect(h: HWND, lp: LPARAM) -> BOOL {
        (*(lp.0 as *mut Vec<HWND>)).push(h);
        true.into()
    }
    let mut out: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(collect), LPARAM(&mut out as *mut _ as isize));
    }
    out
}

/// Fenêtre d'application « normale », comme celles qu'affiche Alt+Tab.
pub fn is_app_window(h: HWND) -> bool {
    unsafe {
        if !IsWindowVisible(h).as_bool() || pid(h) == GetCurrentProcessId() {
            return false;
        }
        let style = GetWindowLongPtrW(h, GWL_STYLE) as u32;
        let ex = GetWindowLongPtrW(h, GWL_EXSTYLE) as u32;
        // Les fenêtres « toujours au premier plan » (vidéo en incrustation…) flottent
        // au-dessus de toutes les scènes, comme sur macOS.
        if style & WS_CHILD.0 != 0 || ex & (WS_EX_NOACTIVATE.0 | WS_EX_TOPMOST.0) != 0 {
            return false;
        }
        let app_window = ex & WS_EX_APPWINDOW.0 != 0;
        if ex & WS_EX_TOOLWINDOW.0 != 0 && !app_window {
            return false;
        }
        let owner = GetWindow(h, GW_OWNER).unwrap_or_default();
        if !owner.is_invalid() && !app_window {
            return false;
        }
    }
    !is_cloaked(h) && !title(h).trim().is_empty() && !IGNORED_CLASSES.contains(&class_name(h).as_str())
}

/// Une vraie appli plein écran (jeu, vidéo…) est-elle au premier plan ? Windows prévient aussi
/// quand c'est le bureau qui prend le premier plan (il couvre tout l'écran) : on l'écarte.
pub fn fullscreen_app_in_front() -> bool {
    fullscreen_monitor().is_some()
}

/// Écran couvert par l'appli plein écran au premier plan, s'il y en a une.
pub fn fullscreen_monitor() -> Option<HMONITOR> {
    unsafe {
        let h = GetForegroundWindow();
        if h.is_invalid() || pid(h) == GetCurrentProcessId() || SHELL_CLASSES.contains(&class_name(h).as_str()) {
            return None;
        }
        let monitor = MonitorFromWindow(h, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(monitor, &mut info);
        covers(window_rect(h), info.rcMonitor).then_some(monitor)
    }
}

/// Une appli plein écran est-elle encore devant tout sur cet écran ? (la première fenêtre
/// qu'on y voit, dans l'ordre Z, le couvre en entier sans être simplement agrandie). Les petites
/// fenêtres « toujours au premier plan » qui flottent par-dessus (vidéo en incrustation…) sont
/// passées.
pub fn fullscreen_window_on(monitor: RECT) -> bool {
    let me = unsafe { GetCurrentProcessId() };
    for h in top_level_windows() {
        unsafe {
            if !IsWindowVisible(h).as_bool() || IsIconic(h).as_bool() || pid(h) == me || is_cloaked(h) {
                continue;
            }
        }
        if SHELL_CLASSES.contains(&class_name(h).as_str()) {
            continue;
        }
        let r = window_rect(h);
        let overlaps = r.left < monitor.right && r.right > monitor.left && r.top < monitor.bottom && r.bottom > monitor.top;
        if !overlaps {
            continue;
        }
        if covers(r, monitor) {
            return !unsafe { IsZoomed(h) }.as_bool();
        }
        let topmost = unsafe { GetWindowLongPtrW(h, GWL_EXSTYLE) } as u32 & WS_EX_TOPMOST.0 != 0;
        if !topmost {
            return false;
        }
    }
    false
}

/// Fenêtres du bureau et de la barre des tâches : elles couvrent l'écran sans être des applis.
const SHELL_CLASSES: [&str; 4] = ["Progman", "WorkerW", "Shell_TrayWnd", "Shell_SecondaryTrayWnd"];

fn covers(r: RECT, m: RECT) -> bool {
    r.left <= m.left && r.top <= m.top && r.right >= m.right && r.bottom >= m.bottom
}

fn window_rect(h: HWND) -> RECT {
    let mut rect = RECT::default();
    unsafe {
        let _ = GetWindowRect(h, &mut rect);
    }
    rect
}

pub fn is_alive(h: HWND) -> bool {
    unsafe { IsWindow(h).as_bool() && IsWindowVisible(h).as_bool() }
}

pub fn is_minimized(h: HWND) -> bool {
    unsafe { IsIconic(h).as_bool() }
}

pub fn is_cloaked(h: HWND) -> bool {
    let mut cloaked = 0u32;
    unsafe {
        DwmGetWindowAttribute(h, DWMWA_CLOAKED, &mut cloaked as *mut u32 as *mut c_void, size_of::<u32>() as u32)
            .is_ok()
            && cloaked != 0
    }
}

pub fn title(h: HWND) -> String {
    let mut buf = [0u16; 512];
    let n = unsafe { GetWindowTextW(h, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

pub fn class_name(h: HWND) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetClassNameW(h, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

pub fn pid(h: HWND) -> u32 {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(h, Some(&mut pid as *mut u32)) };
    pid
}

/// Processus qui « possède » vraiment la fenêtre. Les applis UWP sont hébergées par
/// ApplicationFrameHost : on va chercher le processus de leur fenêtre enfant.
pub fn app_pid(h: HWND) -> u32 {
    unsafe extern "system" fn find(child: HWND, lp: LPARAM) -> BOOL {
        let (host, found) = &mut *(lp.0 as *mut (u32, u32));
        let p = pid(child);
        if p != *host {
            *found = p;
            return false.into();
        }
        true.into()
    }
    let host = pid(h);
    if class_name(h) != "ApplicationFrameWindow" {
        return host;
    }
    let mut data = (host, host);
    unsafe {
        let _ = EnumChildWindows(h, Some(find), LPARAM(&mut data as *mut _ as isize));
    }
    data.1
}

/// Chemin de l'exécutable, en minuscules (sert de clé d'application).
pub fn process_path(pid: u32) -> Option<String> {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let res = QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(process);
        res.ok()?;
        Some(String::from_utf16_lossy(&buf[..len as usize]).to_lowercase())
    }
}

/// `None` si on ne peut pas le savoir (processus protégé ou d'un autre utilisateur).
pub fn is_elevated(pid: Option<u32>) -> Option<bool> {
    unsafe {
        let process = match pid {
            Some(pid) => OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?,
            None => GetCurrentProcess(),
        };
        let mut token = HANDLE::default();
        let opened = OpenProcessToken(process, TOKEN_QUERY, &mut token);
        if pid.is_some() {
            let _ = CloseHandle(process);
        }
        opened.ok()?;
        let mut elevation = TOKEN_ELEVATION::default();
        let mut len = 0u32;
        let res = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elevation as *mut _ as *mut c_void),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        );
        let _ = CloseHandle(token);
        res.ok()?;
        Some(elevation.TokenIsElevated != 0)
    }
}

pub fn icon(h: HWND) -> HICON {
    unsafe {
        for kind in [ICON_BIG, ICON_SMALL2, ICON_SMALL] {
            let mut result = 0usize;
            let ok = SendMessageTimeoutW(
                h,
                WM_GETICON,
                WPARAM(kind as usize),
                LPARAM(0),
                SMTO_ABORTIFHUNG | SMTO_BLOCK,
                100,
                Some(&mut result as *mut usize),
            );
            if ok.0 != 0 && result != 0 {
                return HICON(result as *mut c_void);
            }
        }
        let class_icon = GetClassLongPtrW(h, GCLP_HICON);
        if class_icon != 0 {
            return HICON(class_icon as *mut c_void);
        }
        LoadIconW(None, IDI_APPLICATION).unwrap_or_default()
    }
}

/// Icône de l'application (ressource n° 1 de l'exécutable), à la taille d'une petite icône.
pub fn app_icon() -> HICON {
    unsafe {
        let size = GetSystemMetricsForDpi(SM_CXSMICON, GetDpiForSystem());
        GetModuleHandleW(None)
            .and_then(|module| LoadImageW(module, PCWSTR(1 as _), IMAGE_ICON, size, size, LR_DEFAULTCOLOR))
            .map(|h| HICON(h.0))
            .or_else(|_| LoadIconW(None, IDI_APPLICATION))
            .unwrap_or_default()
    }
}

pub fn show(h: HWND, cmd: SHOW_WINDOW_CMD) {
    unsafe {
        // Asynchrone : une application figée ne doit pas bloquer la nôtre.
        let _ = ShowWindowAsync(h, cmd);
    }
}

pub fn force_foreground(h: HWND) {
    unsafe {
        if GetForegroundWindow() == h || SetForegroundWindow(h).as_bool() {
            return;
        }
        // Windows refuse le premier plan aux processus qui n'ont pas reçu la dernière
        // entrée utilisateur ; un événement souris vide lève ce verrou.
        let input = INPUT { r#type: INPUT_MOUSE, ..Default::default() };
        SendInput(&[input], size_of::<INPUT>() as i32);
        let _ = SetForegroundWindow(h);
    }
}
