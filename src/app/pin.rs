//! Page accrochée à l'encoche (un graphique TradingView, par exemple). Elle tourne dans une
//! fenêtre Chrome à part, garée hors de l'écran, et l'encoche la montre en direct.
//!
//! Garée plutôt que réduite : une fenêtre réduite ne se dessine plus. Chrome, lui, cesse de
//! dessiner une page qu'il croit cachée : on le lance dans son propre profil, sans quoi il
//! rejoindrait le Chrome déjà ouvert et ignorerait nos options, avec cette détection coupée.
//!
//! Déposée sur l'encoche, elle s'épingle au premier plan comme n'importe quelle fenêtre (voir
//! `float`) : elle n'est alors plus garée quand une autre fenêtre passe devant.

use std::path::{Path, PathBuf};

use windows::{
    Wdk::System::Threading::{NtQueryInformationProcess, ProcessCommandLineInformation},
    Win32::{
        System::{
            Com::{CoCreateInstance, CLSCTX_INPROC_SERVER},
            DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard},
            Diagnostics::Debug::MessageBeep,
            Memory::{GlobalLock, GlobalUnlock},
            Ole::CF_UNICODETEXT,
            Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
        },
        UI::Accessibility::{
            CUIAutomation, IUIAutomation, IUIAutomationValuePattern, TreeScope_Descendants, UIA_ControlTypePropertyId,
            UIA_EditControlTypeId, UIA_ValuePatternId,
        },
    },
};

use super::*;

const TIMER_PIN: usize = 7;
pub(super) const HOTKEY_PIN: i32 = 4;
/// Surveillance de la fenêtre (titre, fermeture…), et plus serrée quand elle arrive.
const POLL: u32 = 500;
const POLL_FAST: u32 = 100;
/// Au-delà, on cesse d'attendre la fenêtre d'un Chrome qu'on vient de lancer.
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(30);
/// Distance entre le bord droit des écrans et la fenêtre garée.
const PARK_GAP: i32 = 200;
/// Hauteur de la barre de titre des fenêtres d'appli de Chrome, en pixels à 100 % : on la
/// coupe de l'aperçu.
pub(super) const TITLE_BAR: i32 = 33;

/// Fenêtre garée, sa place à l'écran et si elle était agrandie : à lui rendre quoi qu'il
/// arrive en quittant.
static PARKED: Mutex<Option<(isize, RECT, bool)>> = Mutex::new(None);

pub(super) struct Pin {
    /// Adresse de la page accrochée.
    pub url: Option<String>,
    /// Sa fenêtre, si elle est ouverte.
    pub hwnd: Option<HWND>,
    /// La fenêtre est à l'écran (on la regarde) plutôt que garée.
    pub peek: bool,
    /// Titre de la fenêtre (le symbole et le cours, chez TradingView).
    pub title: String,
    /// Chrome lancé par nous, dont on attend la fenêtre : à quel moment, et faut-il la montrer.
    launching: Option<(Instant, bool)>,
    /// Ancienne fenêtre en train de se fermer, à ne pas reprendre.
    retired: Option<HWND>,
    /// Processus déjà examinés : est-ce notre Chrome ?
    processes: HashMap<u32, bool>,
    polls: u32,
}

impl Pin {
    pub fn new() -> Self {
        Self {
            url: load_url(),
            hwnd: None,
            peek: false,
            title: String::new(),
            launching: None,
            retired: None,
            processes: HashMap::new(),
            polls: 0,
        }
    }
}

/// Cours affiché par TradingView dans le titre : « BTCUSDT 77,902.01 ▲ +0.41% ».
#[derive(Debug, PartialEq)]
pub(super) struct Ticker {
    pub symbol: String,
    pub price: String,
    /// Variation du jour, et si elle est à la hausse.
    pub change: Option<(String, bool)>,
}

pub(super) fn parse_ticker(title: &str) -> Option<Ticker> {
    let words: Vec<&str> = title.split_whitespace().collect();
    let symbol = words.first()?;
    // En français, TradingView sépare les milliers par une espace : « 3 899,44 ».
    let digits = words[1..].iter().take_while(|w| w.starts_with(|c: char| c.is_ascii_digit())).count();
    if digits == 0 {
        return None;
    }
    let price = words[1..=digits].join("\u{a0}");
    let change = match (words.get(digits + 1), words.get(digits + 2)) {
        (Some(&arrow), Some(change)) if arrow == "▲" || arrow == "▼" => Some((change.to_string(), arrow == "▲")),
        _ => None,
    };
    Some(Ticker { symbol: symbol.to_string(), price, change })
}

impl App {
    pub(super) fn handle_pin_message(&mut self, msg: u32, wp: WPARAM) -> bool {
        match msg {
            WM_TIMER if wp.0 == TIMER_PIN => self.pin_poll(),
            WM_HOTKEY if wp.0 == HOTKEY_PIN as usize => {
                let foreground = unsafe { GetForegroundWindow() };
                self.pin_window(foreground);
            }
            _ => return false,
        }
        true
    }

    pub(super) fn enable_pin(&mut self) {
        self.pin.peek = false;
        self.pin_poll();
        // Pour qu'elle soit à jour dès qu'on regarde l'encoche.
        if self.pin.url.is_some() && self.pin.hwnd.is_none() && self.pin.launching.is_none() {
            self.pin_launch(false);
        }
    }

    /// `close` : la fonctionnalité est désactivée, la fenêtre de la page est fermée. Sinon
    /// (on quitte), elle est ramenée à l'écran : plus rien ne l'y ramènerait.
    pub(super) fn disable_pin(&mut self, close: bool) {
        unsafe {
            let _ = KillTimer(self.hwnd, TIMER_PIN);
        }
        if close {
            if let Some(h) = self.find_pin_window() {
                self.pin.hwnd = Some(h);
            }
            self.close_pin_window();
        }
        self.pin.launching = None;
        restore_parked();
    }

    /// Une page est accrochée, et la fonctionnalité en service.
    pub(super) fn pin_shown(&self) -> bool {
        self.features.active(Feature::Pin) && self.pin.url.is_some()
    }

    // --- Reconnaître la fenêtre ----------------------------------------------------------

    /// La fenêtre appartient-elle au Chrome de la page accrochée ? Elle n'entre alors dans
    /// aucune scène.
    pub(super) fn is_pin_window(&mut self, h: HWND) -> bool {
        if self.pin.url.is_none() || win::class_name(h) != "Chrome_WidgetWin_1" {
            return false;
        }
        let pid = win::pid(h);
        *self.pin.processes.entry(pid).or_insert_with(|| is_pin_process(pid))
    }

    /// Fenêtre principale de la page : la plus grande du processus, s'il en a plusieurs
    /// (fenêtre de connexion…).
    fn find_pin_window(&mut self) -> Option<HWND> {
        if let Some(h) = self.pin.hwnd.filter(|&h| win::is_alive(h)) {
            return Some(h);
        }
        let retired = self.pin.retired;
        let mut best: Option<(HWND, i64)> = None;
        for h in win::top_level_windows() {
            if Some(h) == retired || !win::is_app_window(h) || !self.is_pin_window(h) {
                continue;
            }
            let r = anim::window_rect(h);
            let area = (r.right - r.left) as i64 * (r.bottom - r.top) as i64;
            if best.is_none_or(|(_, a)| area > a) {
                best = Some((h, area));
            }
        }
        best.map(|(h, _)| h)
    }

    // --- Surveillance ----------------------------------------------------------------

    fn pin_poll(&mut self) {
        if !self.pin_shown() {
            unsafe {
                let _ = KillTimer(self.hwnd, TIMER_PIN);
            }
            return;
        }
        // Les numéros de processus finissent par resservir : on les réexamine de temps en temps.
        self.pin.polls = self.pin.polls.wrapping_add(1);
        if self.pin.polls % 60 == 0 {
            self.pin.processes.clear();
        }
        if self.pin.retired.is_some_and(|h| !unsafe { IsWindow(h) }.as_bool()) {
            self.pin.retired = None;
        }
        let mut changed = false;
        let found = self.find_pin_window();
        if found != self.pin.hwnd {
            self.pin.hwnd = found;
            self.notch_drop_thumb();
            changed = true;
            match found {
                Some(h) => {
                    // Pas d'animation de Windows quand on la gare ou qu'on la réduit.
                    anim::set_transitions(h, false);
                    let show = self.pin.launching.take().is_some_and(|(_, show)| show);
                    self.pin.peek = show;
                    if show {
                        win::force_foreground(h);
                    }
                }
                None => {
                    self.pin.peek = false;
                    *PARKED.lock().unwrap_or_else(|e| e.into_inner()) = None;
                }
            }
        }
        if self.pin.launching.is_some_and(|(at, _)| at.elapsed() > LAUNCH_TIMEOUT) {
            self.pin.launching = None;
        }
        let mut fast = self.pin.launching.is_some();
        if let Some(h) = self.pin.hwnd {
            if self.float.hwnd == Some(h) {
                // Épinglée : c'est `float` qui la montre ou la réduit.
            } else if win::is_minimized(h) {
                self.pin_minimized(h);
                fast = true;
            } else if !self.pin.peek && !is_parked(h) {
                self.park(h);
            }
            let title = win::title(h);
            if title != self.pin.title {
                self.pin.title = title;
                changed = true;
            }
        } else if !self.pin.title.is_empty() {
            self.pin.title.clear();
            changed = true;
        }
        unsafe {
            SetTimer(self.hwnd, TIMER_PIN, if fast { POLL_FAST } else { POLL }, None);
        }
        if changed {
            self.notch_retarget();
        }
    }

    /// Événement Windows sur la fenêtre accrochée, ou qui la concerne. Renvoie `true` s'il
    /// est traité (il ne doit pas toucher aux scènes).
    pub(super) fn pin_event(&mut self, event: u32, h: HWND) -> bool {
        let Some(pinned) = self.pin.hwnd.filter(|_| self.features.active(Feature::Pin)) else { return false };
        if h != pinned {
            // Une autre appli passe au premier plan : la page retourne au garage, sauf si elle
            // est épinglée.
            if event == EVENT_SYSTEM_FOREGROUND
                && self.pin.peek
                && self.float.hwnd != Some(pinned)
                && win::pid(h) != win::pid(pinned)
                && win::is_app_window(h)
            {
                self.park(pinned);
            }
            return false;
        }
        match event {
            // Choisie avec Alt+Tab : elle revient à l'écran.
            EVENT_SYSTEM_FOREGROUND if !self.pin.peek => self.unpark(false, None),
            EVENT_SYSTEM_MINIMIZESTART => self.pin_minimized(pinned),
            EVENT_OBJECT_HIDE | EVENT_OBJECT_DESTROY => self.pin_poll_soon(),
            _ => {}
        }
        true
    }

    /// Réduite (bouton de réduction, Win + Bas…), la page ne se dessinerait plus : on la
    /// rouvre aussitôt, garée.
    fn pin_minimized(&mut self, h: HWND) {
        win::show(h, SW_SHOWNOACTIVATE);
        self.pin.peek = false;
        self.pin_poll_soon();
    }

    fn pin_poll_soon(&mut self) {
        unsafe {
            SetTimer(self.hwnd, TIMER_PIN, POLL_FAST, None);
        }
    }

    /// Les écrans ont changé : la fenêtre garée est peut-être devenue visible.
    pub(super) fn pin_displays_changed(&mut self) {
        if self.pin_shown() {
            self.pin_poll();
        }
    }

    // --- Garer, montrer ---------------------------------------------------------------

    /// Pousse la fenêtre hors de l'écran, où elle continue de se dessiner.
    fn park(&mut self, h: HWND) {
        self.pin.peek = false;
        if !is_parked(h) {
            unsafe {
                let maximized = IsZoomed(h).as_bool();
                if maximized {
                    // Une fenêtre agrandie ne se déplace pas : on lui rend sa taille normale.
                    let _ = ShowWindow(h, SW_SHOWNOACTIVATE);
                }
                let home = anim::window_rect(h);
                *PARKED.lock().unwrap_or_else(|e| e.into_inner()) = Some((h.0 as isize, home, maximized));
                let x = GetSystemMetrics(SM_XVIRTUALSCREEN) + GetSystemMetrics(SM_CXVIRTUALSCREEN) + PARK_GAP;
                let _ = SetWindowPos(h, None, x, home.top, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
                // Le clavier ne doit pas rester sur une page invisible.
                if GetForegroundWindow() == h {
                    if let Some(next) = self.window_below(h) {
                        win::force_foreground(next);
                    }
                }
            }
        }
    }

    /// Ramène la fenêtre à sa place à l'écran. `screen` : l'écran où on la demande (celui de
    /// l'encoche cliquée…) ; garée depuis un autre écran, elle y arrive, au milieu.
    fn unpark(&mut self, focus: bool, screen: Option<ScreenId>) {
        let Some(h) = self.pin.hwnd else { return };
        self.pin.peek = true;
        let target = screen.and_then(|id| self.screen_index(id)).map(|s| self.screens[s].handle);
        let parked = (*PARKED.lock().unwrap_or_else(|e| e.into_inner())).filter(|p| p.0 == h.0 as isize);
        let elsewhere = parked
            .zip(target)
            .is_some_and(|((_, home, _), monitor)| unsafe { MonitorFromRect(&home, MONITOR_DEFAULTTONEAREST) } != monitor);
        if elsewhere {
            PARKED.lock().unwrap_or_else(|e| e.into_inner()).take();
        } else {
            restore_parked();
        }
        if elsewhere || is_parked(h) {
            // Jamais vue à l'écran (lancée garée), ou ailleurs : au milieu de l'écran demandé (de
            // l'écran principal, à défaut).
            let r = anim::window_rect(h);
            let (w, hh) = (r.right - r.left, r.bottom - r.top);
            let work = target.map_or_else(work_area, monitor_work);
            let (x, y) = ((work.left + work.right - w) / 2, (work.top + work.bottom - hh) / 2);
            unsafe {
                let _ = SetWindowPos(h, None, x.max(work.left), y.max(work.top), 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
                if elsewhere && parked.is_some_and(|p| p.2) {
                    let _ = ShowWindow(h, SW_MAXIMIZE);
                }
            }
        }
        if focus {
            win::force_foreground(h);
        }
    }

    /// Première fenêtre d'appli visible sous `h`, pour lui passer le clavier.
    pub(super) fn window_below(&mut self, h: HWND) -> Option<HWND> {
        let mut next = h;
        loop {
            next = unsafe { GetWindow(next, GW_HWNDNEXT) }.ok()?;
            if win::is_app_window(next) && !win::is_minimized(next) && !self.is_pin_window(next) {
                return Some(next);
            }
        }
    }

    /// Clic sur l'encoche de l'écran `screen` : montre la page sur cet écran, la regare, ou la
    /// rouvre si on l'a fermée. Épinglée, elle passe de l'écran à sa vignette, et inversement.
    pub(super) fn pin_toggle(&mut self, screen: ScreenId) {
        if self.pin.url.is_none() {
            return;
        }
        match self.pin.hwnd {
            None if self.pin.launching.is_none() => self.pin_launch(true),
            None => {}
            Some(h) if self.float.hwnd == Some(h) => self.toggle_float_reduced(),
            Some(h) if self.pin.peek => self.park(h),
            Some(_) => self.unpark(true, Some(screen)),
        }
    }

    // --- Accrocher, décrocher ------------------------------------------------------------

    /// Ctrl + Alt + P : accroche la page affichée dans la fenêtre `h` (ou l'adresse copiée).
    /// Sur la page déjà accrochée, la décroche.
    pub(super) fn pin_window(&mut self, h: HWND) {
        if !self.features.active(Feature::Pin) {
            return;
        }
        if !h.is_invalid() && (self.pin.hwnd == Some(h) || self.is_pin_window(h)) {
            self.unpin();
            return;
        }
        let Some(url) = page_url(h).or_else(clipboard_url) else {
            unsafe {
                let _ = MessageBeep(MB_ICONWARNING);
            }
            return;
        };
        self.close_pin_window();
        save_url(Some(&url));
        self.pin.url = Some(url);
        self.pin.processes.clear();
        self.pin_launch(true);
    }

    pub(super) fn unpin(&mut self) {
        self.close_pin_window();
        save_url(None);
        self.pin.url = None;
        self.pin.title.clear();
        self.pin.launching = None;
        self.pin.processes.clear();
        self.notch_retarget();
    }

    /// Ferme la fenêtre accrochée, ramenée d'abord à l'écran : une page peut demander
    /// confirmation avant de se fermer.
    fn close_pin_window(&mut self) {
        let Some(h) = self.pin.hwnd.take() else { return };
        if self.float.hwnd == Some(h) {
            self.unpin_float();
        }
        self.notch_drop_thumb();
        restore_parked();
        self.pin.peek = false;
        self.pin.retired = Some(h);
        unsafe {
            let _ = PostMessageW(h, WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }

    fn pin_launch(&mut self, show: bool) {
        let (Some(url), Some(chrome), Some(profile)) = (self.pin.url.clone(), chrome_exe(), profile_dir()) else {
            unsafe {
                let _ = MessageBeep(MB_ICONWARNING);
            }
            return;
        };
        let first = !profile.exists();
        disable_translate(&profile);
        let mut command = std::process::Command::new(chrome);
        command.arg(format!("--user-data-dir={}", profile.display())).args([
            "--no-first-run",
            "--no-default-browser-check",
            "--hide-crash-restore-bubble",
            "--disable-features=CalculateNativeWinOcclusion",
        ]);
        // Tout premier lancement : une taille raisonnable (l'aperçu en est le zoom), que Chrome
        // retiendra ensuite.
        if first {
            command.arg("--window-size=1280,800");
        }
        let launched = command.arg(format!("--app={url}")).spawn();
        if launched.is_ok() {
            self.pin.launching = Some((Instant::now(), show));
            self.pin_poll_soon();
        }
    }

    // --- Aperçu ------------------------------------------------------------------------

    /// Partie de la fenêtre à montrer dans l'encoche (sans ses bordures invisibles ni sa
    /// barre de titre), en coordonnées de la fenêtre, et sa taille.
    pub(super) fn pin_source(&self) -> Option<RECT> {
        window_source(self.pin.hwnd?, TITLE_BAR)
    }
}

/// Partie visible d'une fenêtre pour une miniature DWM, en coordonnées de la fenêtre : sans ses
/// bordures invisibles, ni les `title_bar` premiers pixels (à 100 %) de sa barre de titre.
pub(super) fn window_source(h: HWND, title_bar: i32) -> Option<RECT> {
    let window = anim::window_rect(h);
    let mut frame = RECT::default();
    let ok = unsafe {
        DwmGetWindowAttribute(
            h,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut frame as *mut RECT as *mut core::ffi::c_void,
            size_of::<RECT>() as u32,
        )
    }
    .is_ok();
    if !ok {
        frame = window;
    }
    let dpi = unsafe { GetDpiForWindow(h) }.max(96) as i32;
    let source = RECT {
        left: frame.left - window.left,
        top: frame.top - window.top + title_bar * dpi / 96,
        right: frame.right - window.left,
        bottom: frame.bottom - window.top,
    };
    (source.right > source.left && source.bottom > source.top).then_some(source)
}

/// Rend sa place à la fenêtre garée (en quittant, en désactivant…).
pub(super) fn restore_parked() {
    let Some((h, home, maximized)) = PARKED.lock().unwrap_or_else(|e| e.into_inner()).take() else { return };
    let h = HWND(h as _);
    unsafe {
        if !IsWindow(h).as_bool() {
            return;
        }
        let _ = SetWindowPos(h, None, home.left, home.top, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
        if maximized {
            let _ = ShowWindow(h, SW_MAXIMIZE);
        }
    }
}

pub(super) fn is_parked(h: HWND) -> bool {
    let right = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) + GetSystemMetrics(SM_CXVIRTUALSCREEN) };
    anim::window_rect(h).left >= right
}

fn work_area() -> RECT {
    let mut work = RECT::default();
    unsafe {
        let _ = SystemParametersInfoW(SPI_GETWORKAREA, 0, Some(&mut work as *mut RECT as *mut core::ffi::c_void), Default::default());
    }
    work
}

/// Zone de travail d'un écran : sans la barre des tâches, le Dock ni la barre des scènes.
pub(super) fn monitor_work(monitor: HMONITOR) -> RECT {
    let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
    unsafe {
        let _ = GetMonitorInfoW(monitor, &mut info);
    }
    info.rcWork
}

// --- Chrome ------------------------------------------------------------------------------

fn chrome_exe() -> Option<PathBuf> {
    ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"]
        .iter()
        .filter_map(|var| std::env::var(var).ok())
        .map(|dir| Path::new(&dir).join("Google\\Chrome\\Application\\chrome.exe"))
        .find(|p| p.exists())
}

/// Profil de Chrome réservé à la page accrochée (on s'y connecte une fois pour toutes).
fn profile_dir() -> Option<PathBuf> {
    Some(Path::new(&std::env::var("LOCALAPPDATA").ok()?).join("StageManager").join("Chrome"))
}

/// Coupe la traduction dans le profil : sa bulle s'afficherait au bord de l'écran, à côté de
/// la fenêtre garée. Chrome relit ses préférences au démarrage ; on n'y touche que si elles
/// ne disent rien de la traduction.
fn disable_translate(profile: &Path) {
    const OFF: &str = "\"translate\":{\"enabled\":false}";
    let file = profile.join("Default").join("Preferences");
    let text = std::fs::read_to_string(&file).unwrap_or_default();
    let patched = match text.trim_start().strip_prefix('{') {
        _ if text.contains("\"translate\":") => return,
        None if text.trim().is_empty() => format!("{{{OFF}}}"),
        None => return,
        Some(rest) if rest.trim_start().starts_with('}') => format!("{{{OFF}}}"),
        Some(rest) => format!("{{{OFF},{rest}"),
    };
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(file, patched);
}

/// Le processus est-il un Chrome lancé sur notre profil ? On le lit dans sa ligne de commande.
fn is_pin_process(pid: u32) -> bool {
    let (Some(command), Some(profile)) = (command_line(pid), profile_dir()) else { return false };
    let profile = profile.to_string_lossy().to_lowercase();
    command.to_lowercase().contains(&format!("--user-data-dir={profile}"))
}

fn command_line(pid: u32) -> Option<String> {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        // Le système y écrit un UNICODE_STRING suivi du texte : il faut un tampon aligné.
        let mut buf = vec![0u64; 1024];
        let mut len = 0u32;
        let mut status = NtQueryInformationProcess(process, ProcessCommandLineInformation, buf.as_mut_ptr().cast(), (buf.len() * 8) as u32, &mut len);
        if status.is_err() && len as usize > buf.len() * 8 {
            buf = vec![0u64; len as usize / 8 + 1];
            status = NtQueryInformationProcess(process, ProcessCommandLineInformation, buf.as_mut_ptr().cast(), (buf.len() * 8) as u32, &mut len);
        }
        let _ = CloseHandle(process);
        status.ok().ok()?;
        let text = &*(buf.as_ptr() as *const UNICODE_STRING);
        if text.Buffer.is_null() {
            return None;
        }
        Some(String::from_utf16_lossy(std::slice::from_raw_parts(text.Buffer.0, text.Length as usize / 2)))
    }
}

// --- Adresse de la page ------------------------------------------------------------------

/// Adresse affichée dans la barre d'adresse du navigateur (Chrome, Edge, Firefox, Zen…), lue
/// par UI Automation : c'est le premier champ de saisie de la fenêtre.
fn page_url(h: HWND) -> Option<String> {
    if h.is_invalid() {
        return None;
    }
    unsafe {
        let uia: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).ok()?;
        let root = uia.ElementFromHandle(h).ok()?;
        let edit = VARIANT::from(UIA_EditControlTypeId.0);
        let condition = uia.CreatePropertyCondition(UIA_ControlTypePropertyId, &edit).ok()?;
        let field = root.FindFirst(TreeScope_Descendants, &condition).ok()?;
        let value: IUIAutomationValuePattern = field.GetCurrentPatternAs(UIA_ValuePatternId).ok()?;
        normalize_url(&value.CurrentValue().ok()?.to_string(), true)
    }
}

/// Adresse copiée dans le presse-papiers, en dernier recours.
fn clipboard_url() -> Option<String> {
    unsafe {
        OpenClipboard(None).ok()?;
        let text = (|| {
            let data = GetClipboardData(CF_UNICODETEXT.0 as u32).ok()?;
            let memory = HGLOBAL(data.0);
            let p = GlobalLock(memory) as *const u16;
            if p.is_null() {
                return None;
            }
            let mut n = 0;
            while *p.add(n) != 0 && n < 8192 {
                n += 1;
            }
            let text = String::from_utf16_lossy(std::slice::from_raw_parts(p, n));
            let _ = GlobalUnlock(memory);
            Some(text)
        })();
        let _ = CloseClipboard();
        normalize_url(&text?, false)
    }
}

/// Adresse web complète, ou `None`. Les barres d'adresse cachent souvent le « https:// » :
/// `lenient` accepte une adresse sans protocole.
fn normalize_url(text: &str, lenient: bool) -> Option<String> {
    let text = text.trim();
    if text.is_empty() || text.chars().any(char::is_whitespace) {
        return None;
    }
    let lower = text.to_lowercase();
    if lower.starts_with("https://") || lower.starts_with("http://") {
        return Some(text.to_owned());
    }
    let host = text.split('/').next().unwrap_or_default();
    (lenient && !lower.contains("://") && host.contains('.') && !host.starts_with('.')).then(|| format!("https://{text}"))
}

// --- Enregistrement ------------------------------------------------------------------------

/// Adresse de la page accrochée, dans les données de l'utilisateur.
fn url_file() -> Option<PathBuf> {
    Some(Path::new(&std::env::var("APPDATA").ok()?).join("StageManager").join("notch.txt"))
}

fn load_url() -> Option<String> {
    let text = std::fs::read_to_string(url_file()?).ok()?;
    normalize_url(text.lines().next()?, false)
}

fn save_url(url: Option<&str>) {
    let Some(file) = url_file() else { return };
    match url {
        Some(url) => {
            if let Some(dir) = file.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(file, url);
        }
        None => {
            let _ = std::fs::remove_file(file);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cours_de_tradingview() {
        let t = parse_ticker("BTCUSDT 77,902.01 ▲ +0.41%").unwrap();
        assert_eq!(t.symbol, "BTCUSDT");
        assert_eq!(t.price, "77,902.01");
        assert_eq!(t.change, Some(("+0.41%".into(), true)));
        let t = parse_ticker("AAPL 231.5 ▼ −1.2% Ma disposition").unwrap();
        assert_eq!(t.change, Some(("−1.2%".into(), false)));
        assert_eq!(parse_ticker("EURUSD 1.0832").unwrap().change, None);
        let t = parse_ticker("XAUUSD 3\u{202f}899,44 ▲ +0.61% Anonyme").unwrap();
        assert_eq!(t.price, "3\u{a0}899,44", "milliers séparés par une espace, à la française");
        assert_eq!(t.change, Some(("+0.61%".into(), true)));
        assert_eq!(parse_ticker("TradingView"), None);
        assert_eq!(parse_ticker("Graphiques en direct"), None);
    }

    #[test]
    fn adresses() {
        assert_eq!(normalize_url("tradingview.com/chart/abc/", true).as_deref(), Some("https://tradingview.com/chart/abc/"));
        assert_eq!(normalize_url("https://www.tradingview.com/chart/", false).as_deref(), Some("https://www.tradingview.com/chart/"));
        assert_eq!(normalize_url("tradingview.com", false), None, "le presse-papiers exige le protocole");
        assert_eq!(normalize_url("chercher quelque chose", true), None);
        assert_eq!(normalize_url("chrome://settings", true), None);
        assert_eq!(normalize_url("about:blank", true), None);
    }
}
