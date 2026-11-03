//! Minuteur et Pomodoro de l'encoche, lancés par son clic droit. Le compte à rebours s'affiche
//! dans l'encoche fermée, à droite de l'heure, avec un anneau qui se vide ; à la fin, l'encoche
//! s'allonge pour l'annoncer (voir `activity`) et Windows joue son son de notification.
//!
//! Pomodoro : 25 minutes de travail, 5 de pause, et une pause de 15 minutes toutes les quatre
//! séances ; il enchaîne tout seul jusqu'à ce qu'on l'arrête.

use std::f32::consts::PI;

use windows::Win32::Media::Audio::{PlaySoundW, SND_ALIAS, SND_ASYNC, SND_NODEFAULT};

use super::{activity::Activity, dockbar::text_width, *};

const TIMER_COUNTDOWN: usize = 15;
/// Le compte à rebours est relu quatre fois par seconde : il change pile à la seconde.
const REFRESH: u32 = 250;

const FOCUS: u64 = 25;
const BREAK: u64 = 5;
const LONG_BREAK: u64 = 15;
const ROUNDS: u32 = 4;

/// Couleurs : orange pour le minuteur (comme l'app Horloge), tomate pour le travail, vert pour
/// la pause.
const ORANGE: (u8, u8, u8) = (255, 159, 10);
const TOMATO: (u8, u8, u8) = (255, 99, 71);
const GREEN: (u8, u8, u8) = (48, 209, 88);


#[derive(Clone, Copy, PartialEq, Debug)]
enum Phase {
    /// Minuteur simple.
    Simple,
    Focus,
    Break,
}

pub(super) struct Timer {
    phase: Phase,
    total: Duration,
    /// Fin prévue ; en pause, le temps qui restait.
    ends: Instant,
    paused: Option<Duration>,
    /// Séances de travail terminées (Pomodoro).
    rounds: u32,
    /// Dernier texte affiché : on ne redessine qu'à son changement.
    shown: String,
}

/// Ce que l'encoche montre du minuteur.
pub(super) struct TimerView {
    pub text: String,
    /// Part du temps qui reste, de 1 à 0.
    pub left: f32,
    pub color: (u8, u8, u8),
    pub paused: bool,
}

impl Timer {
    fn new(phase: Phase, minutes: u64, rounds: u32) -> Self {
        let total = Duration::from_secs(minutes * 60);
        Self { phase, total, ends: Instant::now() + total, paused: None, rounds, shown: String::new() }
    }

    fn remaining(&self, now: Instant) -> Duration {
        self.paused.unwrap_or_else(|| self.ends.saturating_duration_since(now))
    }

    fn view(&self, now: Instant) -> TimerView {
        let left = self.remaining(now);
        TimerView {
            text: countdown(left),
            left: (left.as_secs_f32() / self.total.as_secs_f32().max(1.0)).clamp(0.0, 1.0),
            color: match self.phase {
                Phase::Simple => ORANGE,
                Phase::Focus => TOMATO,
                Phase::Break => GREEN,
            },
            paused: self.paused.is_some(),
        }
    }
}

/// Temps restant, arrondi à la seconde supérieure : « 24:59 », « 1:05:00 ».
fn countdown(left: Duration) -> String {
    let s = left.as_millis().div_ceil(1000) as u64;
    let (h, m, s) = (s / 3600, s / 60 % 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

impl App {
    pub(super) fn start_timer(&mut self, minutes: u64) {
        self.set_timer(Some(Timer::new(Phase::Simple, minutes, 0)));
    }

    pub(super) fn start_pomodoro(&mut self) {
        self.set_timer(Some(Timer::new(Phase::Focus, FOCUS, 0)));
        self.announce_phase();
    }

    pub(super) fn stop_timer(&mut self) {
        self.set_timer(None);
    }

    pub(super) fn toggle_timer_pause(&mut self) {
        let now = Instant::now();
        if let Some(t) = self.timer.as_mut() {
            match t.paused.take() {
                Some(left) => t.ends = now + left,
                None => t.paused = Some(t.ends.saturating_duration_since(now)),
            }
        }
        self.notch_retarget();
    }

    /// État du minuteur pour le menu : en marche, en pause.
    pub(super) fn timer_state(&self) -> Option<bool> {
        self.timer.as_ref().map(|t| t.paused.is_some())
    }

    fn set_timer(&mut self, timer: Option<Timer>) {
        unsafe {
            if timer.is_some() {
                SetTimer(self.hwnd, TIMER_COUNTDOWN, REFRESH, None);
            } else {
                let _ = KillTimer(self.hwnd, TIMER_COUNTDOWN);
            }
        }
        self.timer = timer;
        // L'encoche fermée s'élargit (ou se resserre) pour le compte à rebours.
        self.notch_retarget();
    }

    pub(super) fn timer_view(&self) -> Option<TimerView> {
        self.timer.as_ref().map(|t| t.view(Instant::now()))
    }

    pub(super) fn handle_timer_message(&mut self, msg: u32, wp: WPARAM) -> bool {
        if msg != WM_TIMER || wp.0 != TIMER_COUNTDOWN {
            return false;
        }
        let now = Instant::now();
        let Some(t) = self.timer.as_mut() else { return true };
        if t.paused.is_none() && t.ends <= now {
            self.timer_done();
            return true;
        }
        let text = countdown(t.remaining(now));
        if text != t.shown {
            // La largeur ne change que quand le temps perd un chiffre (de 10:00 à 9:59).
            let width_changed = text.len() != t.shown.len();
            t.shown = text;
            if width_changed {
                self.notch_retarget();
            } else {
                self.render_notches();
            }
        }
        true
    }

    /// Le temps est écoulé : le minuteur s'arrête, le Pomodoro passe à la suite.
    fn timer_done(&mut self) {
        play_chime();
        let Some(t) = self.timer.as_ref() else { return };
        match t.phase {
            Phase::Simple => {
                let minutes = t.total.as_secs() / 60;
                self.set_timer(None);
                self.show_activity(Activity::Timer { title: "Minuteur terminé".to_owned(), detail: format!("{minutes} min"), color: ORANGE }, 5000);
            }
            Phase::Focus => {
                let rounds = t.rounds + 1;
                let minutes = if rounds.is_multiple_of(ROUNDS) { LONG_BREAK } else { BREAK };
                self.set_timer(Some(Timer::new(Phase::Break, minutes, rounds)));
                self.announce_phase();
            }
            Phase::Break => {
                let rounds = t.rounds;
                self.set_timer(Some(Timer::new(Phase::Focus, FOCUS, rounds)));
                self.announce_phase();
            }
        }
    }

    /// Annonce la séance qui commence.
    fn announce_phase(&mut self) {
        let Some(t) = self.timer.as_ref() else { return };
        let minutes = t.total.as_secs() / 60;
        let (title, color) = match t.phase {
            Phase::Focus => (format!("Au travail · séance {}", t.rounds % ROUNDS + 1), TOMATO),
            _ => ("Pause".to_owned(), GREEN),
        };
        self.show_activity(Activity::Timer { title, detail: format!("{minutes} min"), color }, 3500);
    }
}

fn play_chime() {
    unsafe {
        let _ = PlaySoundW(w!("Notification.Default"), None, SND_ALIAS | SND_ASYNC | SND_NODEFAULT);
    }
}

// --- Dessin ---------------------------------------------------------------------------------

/// Largeur du compte à rebours dans l'encoche fermée : l'anneau, puis le temps.
pub(super) fn timer_width(view: &TimerView, font: HFONT, k: f32) -> i32 {
    (17.0 * k).round() as i32 + text_width(font, &view.text)
}

/// Le compte à rebours, calé à droite sur `right`, dans la bande de hauteur `band`.
pub(super) fn draw_timer(canvas: &mut Canvas, view: &TimerView, right: i32, band: i32, font: HFONT, k: f32) {
    let left = right - timer_width(view, font, k);
    let cy = band as f32 / 2.0;
    let alpha = if view.paused { 130 } else { 255 };
    draw_ring(canvas, (left as f32 + 6.0 * k, cy), 5.0 * k, view.left, view.color, alpha, k);
    let text = RECT { left: left + (17.0 * k).round() as i32, top: 0, right, bottom: band };
    let color = if view.paused { TEXT_DIM } else { rgb(view.color.0, view.color.1, view.color.2) };
    canvas.gdi(|dc| unsafe {
        SetBkMode(dc, TRANSPARENT);
        SelectObject(dc, font);
        draw_text(dc, &view.text, text, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX, color, 0);
    });
}

/// Anneau de progression : le temps qui reste, depuis midi dans le sens des aiguilles.
pub(super) fn draw_ring(canvas: &mut Canvas, center: (f32, f32), radius: f32, left: f32, color: (u8, u8, u8), alpha: u8, k: f32) {
    let width = 1.1 * k;
    canvas.arc_toward(center, radius, -PI / 2.0, PI, width, color, alpha / 4);
    if left > 0.0 {
        let half = left * PI;
        canvas.arc_toward(center, radius, -PI / 2.0 + half, half, width, color, alpha);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compte_a_rebours() {
        assert_eq!(countdown(Duration::from_secs(25 * 60)), "25:00");
        assert_eq!(countdown(Duration::from_millis(1499)), "0:02");
        assert_eq!(countdown(Duration::from_millis(59_001)), "1:00");
        assert_eq!(countdown(Duration::ZERO), "0:00");
        assert_eq!(countdown(Duration::from_secs(3900)), "1:05:00");
    }
}
