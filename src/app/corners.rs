//! Coins arrondis de l'écran, comme sur les Mac : un quart de cercle noir dans chaque coin de
//! l'écran principal, par-dessus tout et transparent à la souris.

use super::*;

/// Rayon des coins, en pixels à 100 %.
const RADIUS: i32 = 20;

pub(super) struct Corners {
    /// En haut à gauche, en haut à droite, en bas à gauche, en bas à droite.
    windows: [HWND; 4],
    shown: bool,
}

impl Corners {
    pub unsafe fn create(instance: HINSTANCE) -> Result<Self> {
        let class = w!("StageManagerCorner");
        let wc = WNDCLASSW { hInstance: instance, lpszClassName: class, lpfnWndProc: Some(corner_proc), ..Default::default() };
        if RegisterClassW(&wc) == 0 {
            return Err(Error::from_win32());
        }
        let mut windows = [HWND::default(); 4];
        for window in &mut windows {
            // Transparente à la souris : les clics passent au travers, même sur le noir.
            *window = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
                class,
                w!("Coin de l'écran"),
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
        }
        Ok(Self { windows, shown: false })
    }

    /// Dessine les coins aux quatre angles de `monitor` et les affiche.
    pub fn show(&mut self, monitor: RECT) {
        let dpi = unsafe {
            let handle = MonitorFromPoint(POINT { x: monitor.left, y: monitor.top }, MONITOR_DEFAULTTOPRIMARY);
            let (mut dpi, mut dpi_y) = (96u32, 96u32);
            let _ = GetDpiForMonitor(handle, MDT_EFFECTIVE_DPI, &mut dpi, &mut dpi_y);
            dpi as i32
        };
        let r = (RADIUS * dpi / 96).max(2);
        let rf = r as f32;
        let places = [
            (POINT { x: monitor.left, y: monitor.top }, (rf, rf)),
            (POINT { x: monitor.right - r, y: monitor.top }, (0.0, rf)),
            (POINT { x: monitor.left, y: monitor.bottom - r }, (rf, 0.0)),
            (POINT { x: monitor.right - r, y: monitor.bottom - r }, (0.0, 0.0)),
        ];
        for (&window, (pos, center)) in self.windows.iter().zip(places) {
            let mut canvas = Canvas::new(r, r);
            canvas.fill_outside_circle(center, rf, (0, 0, 0), 255);
            canvas.present(window, pos);
        }
        self.shown = true;
        self.set_visible(true);
    }

    pub fn hide(&mut self) {
        self.shown = false;
        self.set_visible(false);
    }

    /// Masque ou réaffiche les coins, sans oublier s'ils sont activés (plein écran).
    pub fn set_visible(&self, visible: bool) {
        let cmd = if visible && self.shown { SW_SHOWNOACTIVATE } else { SW_HIDE };
        for &window in &self.windows {
            unsafe {
                let _ = ShowWindow(window, cmd);
            }
        }
        if visible {
            self.raise();
        }
    }

    /// Repasse les coins devant les autres fenêtres « toujours au premier plan ».
    pub fn raise(&self) {
        if !self.shown {
            return;
        }
        for &window in &self.windows {
            unsafe {
                let _ = SetWindowPos(window, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            }
        }
    }
}

unsafe extern "system" fn corner_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    DefWindowProcW(hwnd, msg, wp, lp)
}
