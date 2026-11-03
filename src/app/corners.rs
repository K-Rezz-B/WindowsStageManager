//! Coins arrondis de l'écran, comme sur les Mac : un quart de cercle noir dans chaque coin de
//! chaque écran, par-dessus tout et transparent à la souris.

use super::*;

/// Rayon des coins, en pixels à 100 %.
const RADIUS: i32 = 20;

const CLASS: PCWSTR = w!("StageManagerCorner");

/// Les quatre coins d'un écran.
struct Set {
    monitor: RECT,
    /// En haut à gauche, en haut à droite, en bas à gauche, en bas à droite.
    windows: [HWND; 4],
    /// Une appli plein écran couvre cet écran : ses coins s'effacent.
    hidden: bool,
}

pub(super) struct Corners {
    instance: HINSTANCE,
    sets: Vec<Set>,
    shown: bool,
}

impl Corners {
    pub unsafe fn create(instance: HINSTANCE) -> Result<Self> {
        let wc = WNDCLASSW { hInstance: instance, lpszClassName: CLASS, lpfnWndProc: Some(corner_proc), ..Default::default() };
        if RegisterClassW(&wc) == 0 {
            return Err(Error::from_win32());
        }
        Ok(Self { instance, sets: Vec::new(), shown: false })
    }

    /// Dessine les coins aux quatre angles de chaque écran (`rect`, couvert par une appli plein
    /// écran) et les affiche.
    pub fn show(&mut self, monitors: &[(RECT, bool)]) {
        // Autant de jeux de coins que d'écrans : on en crée ou on en détruit au besoin.
        while self.sets.len() > monitors.len() {
            let set = self.sets.pop().unwrap();
            for window in set.windows {
                unsafe {
                    let _ = DestroyWindow(window);
                }
            }
        }
        while self.sets.len() < monitors.len() {
            let Ok(set) = (unsafe { self.create_set() }) else { break };
            self.sets.push(set);
        }
        for (set, &(monitor, hidden)) in self.sets.iter_mut().zip(monitors) {
            set.monitor = monitor;
            set.hidden = hidden;
            draw(set);
        }
        self.shown = true;
        self.update();
    }

    unsafe fn create_set(&self) -> Result<Set> {
        let mut windows = [HWND::default(); 4];
        for window in &mut windows {
            // Transparente à la souris : les clics passent au travers, même sur le noir.
            *window = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
                CLASS,
                w!("Coin de l'écran"),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                self.instance,
                None,
            )?;
        }
        Ok(Set { monitor: RECT::default(), windows, hidden: false })
    }

    pub fn hide(&mut self) {
        self.shown = false;
        self.update();
    }

    /// Masque ou réaffiche les coins de l'écran `monitor` (plein écran), sans oublier s'ils sont
    /// activés.
    pub fn set_visible(&mut self, monitor: RECT, visible: bool) {
        for set in self.sets.iter_mut().filter(|set| set.monitor == monitor) {
            set.hidden = !visible;
        }
        self.update();
    }

    fn update(&self) {
        for set in &self.sets {
            let cmd = if self.shown && !set.hidden { SW_SHOWNOACTIVATE } else { SW_HIDE };
            for &window in &set.windows {
                unsafe {
                    let _ = ShowWindow(window, cmd);
                }
            }
        }
        self.raise();
    }

    /// Repasse les coins devant les autres fenêtres « toujours au premier plan ».
    pub fn raise(&self) {
        if !self.shown {
            return;
        }
        for set in self.sets.iter().filter(|set| !set.hidden) {
            for &window in &set.windows {
                unsafe {
                    let _ = SetWindowPos(window, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
                }
            }
        }
    }
}

/// Dessine les quatre coins de `set` à l'échelle de son écran.
fn draw(set: &Set) {
    let monitor = set.monitor;
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
    for (&window, (pos, center)) in set.windows.iter().zip(places) {
        let mut canvas = Canvas::new(r, r);
        canvas.fill_outside_circle(center, rf, (0, 0, 0), 255);
        canvas.present(window, pos);
    }
}

unsafe extern "system" fn corner_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    DefWindowProcW(hwnd, msg, wp, lp)
}
