//! Les écrans : chacun a sa barre des scènes, sa barre d'applis, son overlay pour les animations
//! et son encoche.
//! Ils suivent les branchements et débranchements, et chacun s'efface devant une appli plein
//! écran qui le couvre.

use super::*;

pub(super) const BAR_CLASS: PCWSTR = w!("StageManagerBar");
pub(super) const NOTCH_CLASS: PCWSTR = w!("StageManagerNotch");
pub(super) const APPS_CLASS: PCWSTR = w!("StageManagerApps");

/// Un écran tel que Windows le décrit.
struct Monitor {
    /// Nom du périphérique (« \\.\DISPLAY1 ») : il ne change pas quand on rebranche un autre écran.
    device: String,
    handle: HMONITOR,
    rect: RECT,
    primary: bool,
    dpi: i32,
}

/// Les écrans branchés, le principal en premier puis de gauche à droite.
fn monitors() -> Vec<Monitor> {
    unsafe extern "system" fn collect(handle: HMONITOR, _: HDC, _: *mut RECT, lp: LPARAM) -> BOOL {
        let out = &mut *(lp.0 as *mut Vec<Monitor>);
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
        if GetMonitorInfoW(handle, &mut info.monitorInfo).as_bool() {
            let len = info.szDevice.iter().position(|&c| c == 0).unwrap_or(info.szDevice.len());
            let (mut dpi, mut dpi_y) = (96u32, 96u32);
            let _ = GetDpiForMonitor(handle, MDT_EFFECTIVE_DPI, &mut dpi, &mut dpi_y);
            out.push(Monitor {
                device: String::from_utf16_lossy(&info.szDevice[..len]),
                handle,
                rect: info.monitorInfo.rcMonitor,
                primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
                dpi: dpi as i32,
            });
        }
        true.into()
    }
    let mut out: Vec<Monitor> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(HDC::default(), None, Some(collect), LPARAM(&mut out as *mut _ as isize));
    }
    out.sort_by_key(|m| (!m.primary, m.rect.left, m.rect.top));
    out
}

pub(super) struct Screen {
    pub id: ScreenId,
    device: String,
    pub handle: HMONITOR,
    pub rect: RECT,
    pub dpi: i32,
    pub primary: bool,
    /// Une appli plein écran (jeu, vidéo) le couvre : sa barre et son encoche s'effacent.
    pub fullscreen: bool,
    /// La barre des scènes de cet écran.
    pub hwnd: HWND,
    /// Fenêtre transparente qui porte les miniatures animées.
    pub overlay: HWND,
    /// Position de la barre des scènes (en haut, près de l'encoche), quand elles sont en service.
    pub appbar: Option<RECT>,
    /// Bande réservée en haut de l'écran pour l'encoche : les fenêtres (agrandies ou non)
    /// s'arrêtent dessous au lieu de passer derrière elle.
    pub notch_band: Option<RECT>,
    /// La barre est calée contre l'encoche (sinon centrée en haut de l'écran).
    pub bar_anchored: bool,
    /// La barre d'applis de cet écran, à gauche de l'encoche, et sa position à l'écran.
    pub apps: HWND,
    pub apps_rect: RECT,
    /// Les contrôles rapides de cet écran, à droite de l'encoche (voir `quickbar`).
    pub quick: HWND,
    /// Largeur de la pastille au début et à la fin de son animation.
    pub pill_from: i32,
    pub pill_to: i32,
    /// Police des étiquettes de la barre d'applis, à l'échelle de l'écran.
    pub font: HFONT,
    pub cards: Vec<Card>,
    /// Carte de la scène qui revient à l'écran : elle s'efface pendant que ses fenêtres partent.
    pub departing: Option<Card>,
    pub layout_key: LayoutKey,
    pub hover: Option<u64>,
    /// Icône survolée juste avant : elle reprend sa taille pendant que l'autre grossit.
    pub hover_prev: Option<u64>,
    pub hover_start: Option<Instant>,
    /// La pastille rentre dans le bord de l'écran pendant que l'encoche est ouverte.
    pub veiled: bool,
    pub veil_from: f64,
    pub veil_start: Option<Instant>,
    pub tracking_mouse: bool,
    /// Après un changement de scène sur cet écran, on ignore les événements parasites qu'il
    /// provoque.
    pub guard_until: Instant,
    pub switch: Option<Switch>,
    pub slide_start: Option<Instant>,
    pub arriving: Option<u64>,
    pub drag: Option<Drag>,
    /// Point de départ imposé au prochain changement de scène (la carte qu'on vient de lâcher).
    pub drag_source: Option<RECT>,
    /// Positions à rendre aux fenêtres détachées une fois réduites (voir `detach`).
    pub pending_placements: Vec<WindowDrag>,
    /// L'encoche en haut de cet écran.
    pub notch: NotchUi,
}

impl Screen {
    unsafe fn create(id: ScreenId, m: &Monitor) -> Result<Self> {
        let instance: HINSTANCE = GetModuleHandleW(None)?.into();
        let popup = |class: PCWSTR, title: PCWSTR| {
            CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
                class,
                title,
                WS_POPUP,
                // Posée sur son écran, même cachée : Windows sait ainsi à quel écran elle appartient.
                m.rect.left,
                m.rect.top,
                0,
                0,
                None,
                None,
                instance,
                None,
            )
        };
        let hwnd = popup(BAR_CLASS, w!("Stage Manager"))?;
        let notch = popup(NOTCH_CLASS, w!("Encoche"))?;
        let apps = popup(APPS_CLASS, w!("Applis"))?;
        let quick = popup(super::QUICK_CLASS, w!("Contrôles rapides"))?;
        let overlay = anim::create_overlay(instance)?;
        Ok(Self {
            id,
            device: m.device.clone(),
            handle: m.handle,
            rect: m.rect,
            dpi: m.dpi,
            primary: m.primary,
            fullscreen: false,
            hwnd,
            overlay,
            appbar: None,
            notch_band: None,
            bar_anchored: false,
            apps,
            apps_rect: RECT::default(),
            quick,
            pill_from: 0,
            pill_to: 0,
            // Inter, la police libre la plus proche de celle d'Apple (voir `load_fonts`).
            font: make_font("Inter", 12, 400, m.dpi),
            cards: Vec::new(),
            departing: None,
            layout_key: Vec::new(),
            hover: None,
            hover_prev: None,
            hover_start: None,
            veiled: false,
            veil_from: 1.0,
            veil_start: None,
            tracking_mouse: false,
            guard_until: Instant::now(),
            switch: None,
            slide_start: None,
            arriving: None,
            drag: None,
            drag_source: None,
            pending_placements: Vec::new(),
            notch: NotchUi::new(notch),
        })
    }

    unsafe fn destroy(self) {
        let _ = DestroyWindow(self.hwnd);
        let _ = DestroyWindow(self.notch.hwnd);
        let _ = DestroyWindow(self.apps);
        let _ = DestroyWindow(self.quick);
        let _ = DestroyWindow(self.overlay);
        if !self.font.is_invalid() {
            let _ = DeleteObject(self.font);
        }
    }
}

impl App {
    pub(super) fn screen_index(&self, id: ScreenId) -> Option<usize> {
        self.screens.iter().position(|s| s.id == id)
    }

    pub(super) fn bar_index(&self, hwnd: HWND) -> Option<usize> {
        self.screens.iter().position(|s| s.hwnd == hwnd)
    }

    pub(super) fn apps_index(&self, hwnd: HWND) -> Option<usize> {
        self.screens.iter().position(|s| s.apps == hwnd)
    }

    pub(super) fn notch_index(&self, hwnd: HWND) -> Option<usize> {
        self.screens.iter().position(|s| s.notch.hwnd == hwnd)
    }

    /// Écran où se trouve une fenêtre (pour une fenêtre réduite, celui où elle reviendra).
    pub(super) fn screen_at(&self, h: HWND) -> Option<ScreenId> {
        let handle = unsafe { MonitorFromWindow(h, MONITOR_DEFAULTTONEAREST) };
        self.screens.iter().find(|s| s.handle == handle).or(self.screens.first()).map(|s| s.id)
    }

    /// Un changement de scène vient d'avoir lieu sur cet écran.
    pub(super) fn guarded(&self, screen: ScreenId) -> bool {
        self.screen_index(screen).is_some_and(|s| Instant::now() < self.screens[s].guard_until)
    }

    /// Relit les écrans : ceux qu'on vient de brancher reçoivent une barre et une encoche ;
    /// les scènes de ceux qu'on a débranchés passent sur l'écran principal.
    pub(super) fn update_screens(&mut self) {
        let monitors = monitors();
        let Some(main) = monitors.first() else { return };
        for m in &monitors {
            if self.screens.iter().any(|s| s.device == m.device) {
                continue;
            }
            self.next_screen += 1;
            let Ok(screen) = (unsafe { Screen::create(self.next_screen, m) }) else { continue };
            self.screens.push(screen);
            self.register_appbar(self.screens.len() - 1);
        }
        let target = self.screens.iter().find(|s| s.device == main.device).map(|s| s.id);
        let mut s = 0;
        while s < self.screens.len() {
            if monitors.iter().any(|m| m.device == self.screens[s].device) {
                s += 1;
            } else {
                self.remove_screen(s, target);
            }
        }
        // Le principal en premier, comme les écrans de Windows.
        self.screens.sort_by_key(|sc| monitors.iter().position(|m| m.device == sc.device));
        for sc in &mut self.screens {
            let Some(m) = monitors.iter().find(|m| m.device == sc.device) else { continue };
            let moved = sc.rect != m.rect || sc.dpi != m.dpi;
            if sc.dpi != m.dpi {
                unsafe {
                    let _ = DeleteObject(sc.font);
                    sc.font = make_font("Inter", 12, 400, m.dpi);
                }
            }
            sc.handle = m.handle;
            sc.rect = m.rect;
            sc.dpi = m.dpi;
            sc.primary = m.primary;
            if moved && sc.notch_band.is_some() {
                sc.notch_band = Some(RECT::default());
            }
            if moved && sc.appbar.is_some() {
                // Place réservée à recalculer (et police à refaire, si l'échelle a changé).
                sc.appbar = Some(RECT::default());
            }
        }
        self.monitor = main.rect;
        for s in 0..self.screens.len() {
            if self.features.scenes {
                self.show_bar(s);
            }
            self.place_appbar(s);
            if self.features.notch {
                self.enable_notch_on(s);
            }
            self.place_apps(s);
            self.place_quick(s);
        }
        if self.features.corners {
            self.show_corners();
        }
        self.pin_displays_changed();
        // La vignette de la fenêtre épinglée regagne son coin (celui de l'écran principal, si
        // son écran est débranché).
        self.render_vignette();
        self.refresh_all();
    }

    /// Un écran a été débranché : ses scènes passent sur l'écran `target`, ses fenêtres
    /// disparaissent.
    fn remove_screen(&mut self, s: usize, target: Option<ScreenId>) {
        self.cancel_drag(s);
        self.finish_switch(s);
        self.drop_notch_thumb(s);
        self.unregister_appbar(s);
        let id = self.screens[s].id;
        if self.drop_target == Some(id) {
            self.drop_target = None;
        }
        let screen = self.screens.remove(s);
        unsafe { screen.destroy() };
        if let Some(target) = target.filter(|_| self.features.scenes) {
            let t = self.model.move_screen(id, target);
            self.apply_now(t);
        }
    }

    /// Une appli passe en plein écran, ou en sort. Windows ne dit pas toujours sur quel écran :
    /// c'est celui que couvre la fenêtre au premier plan ; un écran déjà effacé le reste tant
    /// qu'une appli plein écran est devant lui.
    pub(super) fn on_fullscreen(&mut self, opened: bool) {
        let front = if opened { win::fullscreen_monitor() } else { None };
        for s in 0..self.screens.len() {
            let sc = &self.screens[s];
            let fullscreen = front == Some(sc.handle) || (sc.fullscreen && win::fullscreen_window_on(sc.rect));
            self.set_fullscreen(s, fullscreen);
        }
    }

    fn set_fullscreen(&mut self, s: usize, fullscreen: bool) {
        self.screens[s].fullscreen = fullscreen;
        if self.features.scenes {
            unsafe {
                let _ = ShowWindow(self.screens[s].hwnd, if fullscreen { SW_HIDE } else { SW_SHOWNOACTIVATE });
            }
        }
        self.place_apps(s);
        self.place_quick(s);
        // L'encoche suit la barre de son écran (sa bande reste réservée). La vignette
        // de la fenêtre épinglée aussi, si elle est dans son coin.
        self.notch_fullscreen(s, fullscreen);
        self.render_vignette();
        // Les coins aussi : posés sur un jeu, ils l'obligeraient à passer par la composition du
        // bureau (un peu plus de latence).
        let rect = self.screens[s].rect;
        self.corners.set_visible(rect, !fullscreen);
    }

    /// Pose les coins arrondis aux angles de chaque écran.
    pub(super) fn show_corners(&mut self) {
        let monitors: Vec<(RECT, bool)> = self.screens.iter().map(|sc| (sc.rect, sc.fullscreen)).collect();
        self.corners.show(&monitors);
    }
}
