//! Le lanceur façon Spotlight à l'écran : Alt + Espace, on tape, Entrée lance l'appli.

use std::collections::HashSet;

use super::{dockbar::text_width, *};
use crate::{
    canvas::Dib,
    launcher::{self, Entry},
    shell,
};

pub(super) const WM_LAUNCHER_APPS: u32 = WM_APP + 9;
pub(super) const WM_LAUNCHER_ICON: u32 = WM_APP + 10;
pub(super) const HOTKEY_LAUNCHER: i32 = 3;
const TIMER_CARET: usize = 4;
const MAX_RESULTS: usize = 7;

pub(super) static LAUNCHER: AtomicIsize = AtomicIsize::new(0);

pub(super) struct LauncherUi {
    pub hwnd: HWND,
    open: bool,
    query: String,
    results: Vec<usize>,
    selected: usize,
    entries: Vec<Entry>,
    apps_loaded: Arc<Mutex<Option<Vec<(String, String)>>>>,
    loader: shell::IconLoader,
    icons: HashMap<String, Rc<Image>>,
    requested: HashSet<String>,
    /// Capture floutée de l'écran derrière le lanceur, prise à l'ouverture : le fond du verre.
    backdrop: Option<Image>,
    window: RECT,
    dpi: i32,
    caret: bool,
    font_query: HFONT,
    font_row: HFONT,
    /// Fenêtre au premier plan avant l'ouverture, qu'on lui rend si on ferme sans rien lancer.
    previous: HWND,
}

impl LauncherUi {
    pub fn new(hwnd: HWND) -> Self {
        Self {
            hwnd,
            open: false,
            query: String::new(),
            results: Vec::new(),
            selected: 0,
            entries: Vec::new(),
            apps_loaded: Arc::new(Mutex::new(None)),
            loader: shell::IconLoader::start(hwnd, WM_LAUNCHER_ICON),
            icons: HashMap::new(),
            requested: HashSet::new(),
            backdrop: None,
            window: RECT::default(),
            dpi: 96,
            caret: true,
            font_query: HFONT::default(),
            font_row: HFONT::default(),
            previous: HWND::default(),
        }
    }

    /// Relit la liste des applis installées dans un fil à part (COM).
    fn load_apps(&self) {
        let shared = Arc::clone(&self.apps_loaded);
        let notify = self.hwnd.0 as isize;
        std::thread::spawn(move || {
            unsafe {
                let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
            }
            let apps = shell::installed_apps();
            if let Ok(mut slot) = shared.lock() {
                *slot = Some(apps);
            }
            unsafe {
                let _ = PostMessageW(HWND(notify as _), WM_LAUNCHER_APPS, WPARAM(0), LPARAM(0));
            }
        });
    }

    fn s(&self, v: i32) -> i32 {
        v * self.dpi / 96
    }

    fn field_height(&self) -> i32 {
        self.s(60)
    }

    fn row_height(&self) -> i32 {
        self.s(46)
    }

    /// Plaque de verre : le champ de recherche, plus les résultats s'il y en a.
    fn panel(&self) -> RECT {
        let w = self.window.right - self.window.left;
        let rows = self.results.len() as i32;
        let list = if rows > 0 { rows * self.row_height() + 2 * self.s(8) } else { 0 };
        let margin = self.s(16);
        RECT { left: margin, top: margin, right: w - margin, bottom: margin + self.field_height() + list }
    }

    fn row_rect(&self, i: usize) -> RECT {
        let p = self.panel();
        let top = p.top + self.field_height() + self.s(8) + i as i32 * self.row_height();
        RECT { left: p.left + self.s(8), top, right: p.right - self.s(8), bottom: top + self.row_height() }
    }

    fn row_at(&self, pt: POINT) -> Option<usize> {
        (0..self.results.len()).find(|&i| contains(self.row_rect(i), pt))
    }
}

impl App {
    pub(super) fn handle_launcher_message(&mut self, msg: u32, wp: WPARAM) -> bool {
        match msg {
            WM_HOTKEY if wp.0 == HOTKEY_LAUNCHER as usize => {
                if self.launcher.open {
                    self.close_launcher(true);
                } else {
                    self.open_launcher();
                }
            }
            WM_LAUNCHER_APPS => {
                let apps = self.launcher.apps_loaded.lock().ok().and_then(|mut a| a.take());
                if let Some(apps) = apps {
                    self.launcher.entries = apps.iter().map(|(name, path)| Entry::new(name, path)).collect();
                    self.update_results();
                }
            }
            WM_LAUNCHER_ICON => {
                for loaded in self.launcher.loader.take() {
                    if let Some(icon) = loaded.icon {
                        self.launcher.icons.insert(loaded.path, Rc::new(icon));
                    }
                }
                self.render_launcher();
            }
            WM_TIMER if wp.0 == TIMER_CARET => {
                self.launcher.caret = !self.launcher.caret;
                self.render_launcher();
            }
            _ => return false,
        }
        true
    }

    pub(super) fn preload_launcher(&self) {
        self.launcher.load_apps();
    }

    fn open_launcher(&mut self) {
        let mon = self.monitor;
        let l = &mut self.launcher;
        unsafe {
            let handle = MonitorFromPoint(POINT::default(), MONITOR_DEFAULTTOPRIMARY);
            let (mut dpi, mut dpi_y) = (96u32, 96u32);
            let _ = GetDpiForMonitor(handle, MDT_EFFECTIVE_DPI, &mut dpi, &mut dpi_y);
            l.dpi = dpi as i32;
        }
        let (w, h) = (l.s(680) + 2 * l.s(16), 2 * l.s(16) + l.field_height() + MAX_RESULTS as i32 * l.row_height() + 2 * l.s(8));
        let left = (mon.left + mon.right - w) / 2;
        let top = mon.top + (mon.bottom - mon.top) / 5;
        l.window = RECT { left, top, right: left + w, bottom: top + h };
        l.backdrop = capture_screen(l.window, l.s(18));
        l.query.clear();
        l.results.clear();
        l.selected = 0;
        l.caret = true;
        l.open = true;
        unsafe {
            for font in [l.font_query, l.font_row] {
                if !font.is_invalid() {
                    let _ = DeleteObject(font);
                }
            }
            l.font_query = make_font("Inter", 18, 400, l.dpi);
            l.font_row = make_font("Inter", 13, 400, l.dpi);
            l.previous = GetForegroundWindow();
        }
        // La liste des applis a pu changer depuis la dernière fois.
        l.load_apps();
        self.render_launcher();
        unsafe {
            let _ = ShowWindow(self.launcher.hwnd, SW_SHOW);
            // On a reçu le raccourci : Windows nous laisse prendre le premier plan (et le clavier).
            let _ = SetForegroundWindow(self.launcher.hwnd);
            SetTimer(self.hwnd, TIMER_CARET, 530, None);
        }
    }

    /// Ferme le lanceur ; `give_back` rend le premier plan à la fenêtre d'avant.
    fn close_launcher(&mut self, give_back: bool) {
        if !self.launcher.open {
            return;
        }
        self.launcher.open = false;
        self.launcher.backdrop = None;
        unsafe {
            let _ = KillTimer(self.hwnd, TIMER_CARET);
            let _ = ShowWindow(self.launcher.hwnd, SW_HIDE);
        }
        let previous = self.launcher.previous;
        if give_back && !previous.is_invalid() {
            win::force_foreground(previous);
        }
    }

    fn update_results(&mut self) {
        let l = &mut self.launcher;
        l.results = launcher::search(&l.entries, &l.query, MAX_RESULTS);
        l.selected = l.selected.min(l.results.len().saturating_sub(1));
        let paths: Vec<String> = l.results.iter().map(|&i| l.entries[i].path.clone()).collect();
        for path in paths {
            if l.requested.insert(path.clone()) {
                l.loader.request(&path);
            }
        }
        self.render_launcher();
    }

    fn launch_selected(&mut self, index: usize) {
        let Some(&entry) = self.launcher.results.get(index) else { return };
        let path = self.launcher.entries[entry].path.clone();
        self.close_launcher(false);
        shell::open(&path);
    }

    /// Clavier et souris dans la fenêtre du lanceur.
    pub(super) fn launcher_input(&mut self, msg: u32, wp: WPARAM, lp: LPARAM) {
        if !self.launcher.open {
            return;
        }
        match msg {
            WM_CHAR => {
                match wp.0 as u32 {
                    8 => {
                        self.launcher.query.pop();
                    }
                    // Ctrl + Retour arrière : efface tout.
                    127 => self.launcher.query.clear(),
                    13 => return self.launch_selected(self.launcher.selected),
                    27 => return self.close_launcher(true),
                    c if c >= 32 => {
                        if let Some(c) = char::from_u32(c) {
                            self.launcher.query.push(c);
                        }
                    }
                    _ => return,
                }
                self.launcher.selected = 0;
                self.launcher.caret = true;
                self.update_results();
            }
            WM_KEYDOWN => {
                let n = self.launcher.results.len();
                let key = wp.0 as u16;
                if key == VK_DOWN.0 && n > 0 {
                    self.launcher.selected = (self.launcher.selected + 1) % n;
                } else if key == VK_UP.0 && n > 0 {
                    self.launcher.selected = (self.launcher.selected + n - 1) % n;
                } else {
                    return;
                }
                self.render_launcher();
            }
            // Un clic ailleurs fait perdre le premier plan au lanceur : il se ferme.
            WM_ACTIVATE if (wp.0 & 0xFFFF) as u32 == WA_INACTIVE => self.close_launcher(false),
            WM_MOUSEMOVE => {
                if let Some(row) = self.launcher.row_at(point(lp)) {
                    if row != self.launcher.selected {
                        self.launcher.selected = row;
                        self.render_launcher();
                    }
                }
            }
            WM_LBUTTONUP => {
                if let Some(row) = self.launcher.row_at(point(lp)) {
                    self.launch_selected(row);
                }
            }
            _ => {}
        }
    }

    fn render_launcher(&self) {
        let l = &self.launcher;
        if !l.open {
            return;
        }
        let s = |v: i32| l.s(v);
        let (w, h) = (l.window.right - l.window.left, l.window.bottom - l.window.top);
        let panel = l.panel();
        let white = (255, 255, 255);
        let mut canvas = Canvas::new(w, h);
        // Ombre, puis plaque de verre sombre (comme Spotlight en mode sombre).
        canvas.round_rect(offset(inflate(panel, s(4)), 0, s(8)), s(24), (0, 0, 0), 40);
        canvas.round_rect(offset(inflate(panel, s(1)), 0, s(3)), s(20), (0, 0, 0), 60);
        let look = Glass { frost: 0.06, darken: 0.42, refraction: s(12) as f32, bevel: s(16) as f32, rim: 0.7 };
        match &l.backdrop {
            Some(backdrop) => canvas.glass(panel, s(18) as f32, backdrop, (0, 0), &look),
            None => canvas.round_rect(panel, s(18), (30, 30, 36), 235),
        }
        // Loupe.
        let field_cy = (panel.top + l.field_height() / 2) as f32;
        let k = l.dpi as f32 / 96.0;
        let (lx, ly) = (panel.left as f32 + 26.0 * k, field_cy - 2.0 * k);
        canvas.arc((lx, ly), 7.0 * k, std::f32::consts::PI, 2.2 * k, white, 200);
        let (hx, hy, t) = (lx + 5.5 * k, ly + 5.5 * k, 1.2 * k);
        let handle = [(hx - t, hy + t), (hx + t, hy - t), (hx + 5.5 * k + t, hy + 5.5 * k - t), (hx + 5.5 * k - t, hy + 5.5 * k + t)];
        canvas.fill_convex(&handle, white, 200);
        let text_left = panel.left + s(48);
        let field = RECT { left: text_left, top: panel.top, right: panel.right - s(16), bottom: panel.top + l.field_height() };
        // Curseur clignotant après le texte.
        if l.caret {
            let x = text_left + if l.query.is_empty() { 0 } else { text_width(l.font_query, &l.query) + s(1) };
            let caret = RECT { left: x, top: field.top + s(18), right: x + s(2).max(2), bottom: field.bottom - s(18) };
            canvas.round_rect(caret, 1, white, 230);
        }
        if !l.results.is_empty() {
            let line = RECT { left: panel.left + s(16), top: field.bottom, right: panel.right - s(16), bottom: field.bottom + 1 };
            canvas.round_rect(line, 0, white, 40);
            canvas.round_rect(l.row_rect(l.selected), s(10), white, 45);
        }
        let icon = s(32);
        for (row, &i) in l.results.iter().enumerate() {
            let r = l.row_rect(row);
            let top = (r.top + r.bottom - icon) / 2;
            let rect = RECT { left: r.left + s(10), top, right: r.left + s(10) + icon, bottom: top + icon };
            if let Some(img) = l.icons.get(&l.entries[i].path) {
                let q = [
                    (rect.left as f32, rect.top as f32),
                    (rect.right as f32, rect.top as f32),
                    (rect.right as f32, rect.bottom as f32),
                    (rect.left as f32, rect.bottom as f32),
                ];
                canvas.draw_quad(img, q, 255, 0);
            }
        }
        canvas.gdi(|dc| unsafe {
            SetBkMode(dc, TRANSPARENT);
            let line = DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX;
            SelectObject(dc, l.font_query);
            if l.query.is_empty() {
                draw_text(dc, "Rechercher une app", field, line, TEXT_DIM, 0);
            } else {
                draw_text(dc, &l.query, field, line, TEXT, 0);
            }
            SelectObject(dc, l.font_row);
            for (row, &i) in l.results.iter().enumerate() {
                let r = l.row_rect(row);
                let text = RECT { left: r.left + s(10) + icon + s(14), right: r.right - s(12), ..r };
                draw_text(dc, &l.entries[i].name, text, line, TEXT, 0);
            }
        });
        canvas.present(l.hwnd, POINT { x: l.window.left, y: l.window.top });
    }
}

/// Copie floutée d'une zone de l'écran (avant d'y afficher le lanceur).
fn capture_screen(rect: RECT, blur: i32) -> Option<Image> {
    let (w, h) = (rect.right - rect.left, rect.bottom - rect.top);
    unsafe {
        let screen = GetDC(None);
        let dib = Dib::new(screen, w, h);
        let image = dib.map(|dib| {
            let _ = BitBlt(dib.dc, 0, 0, w, h, screen, rect.left, rect.top, SRCCOPY | CAPTUREBLT);
            let _ = GdiFlush();
            Image { w, h, px: dib.pixels().iter().map(|p| p | 0xFF00_0000).collect() }
        });
        ReleaseDC(None, screen);
        image.map(|mut img| {
            img.blur(blur);
            img
        })
    }
}
