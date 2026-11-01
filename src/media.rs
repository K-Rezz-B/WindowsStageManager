//! Lecteur de musique : le morceau en cours dans Windows (Spotify, navigateur, Deezer…),
//! lu via les commandes multimédias du système — celles du petit lecteur du panneau de volume.
//!
//! Tout se passe dans un fil à part : les API WinRT sont asynchrones et on ne veut pas
//! bloquer la barre. Le fil prévient la barre par un message quand quelque chose change.

use std::{
    sync::{
        mpsc::{self, RecvTimeoutError},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};

use windows::{
    core::*,
    Graphics::Imaging::*,
    Media::Control::*,
    Storage::Streams::*,
    Win32::{
        Foundation::*,
        System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED},
        UI::WindowsAndMessaging::PostMessageW,
    },
};

use crate::canvas::Image;

/// Taille de la pochette décodée, en pixels (affichée plus petite, pour la netteté).
const ART_SIZE: u32 = 128;

#[derive(Clone)]
pub struct NowPlaying {
    pub title: String,
    pub artist: String,
    pub playing: bool,
    pub art: Option<Arc<Image>>,
    pub can_previous: bool,
    pub can_next: bool,
    /// Position et durée du morceau, si le lecteur les donne.
    pub timeline: Option<Timeline>,
}

/// Où en est le morceau. Le lecteur ne prévient qu'aux changements (lecture, pause, saut) :
/// entre deux, on calcule la position à partir de l'heure.
#[derive(Clone, Copy, PartialEq)]
pub struct Timeline {
    /// Position au moment `updated`, en secondes.
    pub position: f64,
    pub duration: f64,
    /// Heure de la dernière mise à jour (FILETIME : centaines de nanosecondes depuis 1601).
    pub updated: i64,
}

impl NowPlaying {
    fn same_as(&self, other: &NowPlaying) -> bool {
        self.title == other.title
            && self.artist == other.artist
            && self.playing == other.playing
            && self.art.is_some() == other.art.is_some()
            && self.can_previous == other.can_previous
            && self.can_next == other.can_next
            && self.timeline == other.timeline
    }

    /// Position et durée du morceau en secondes, à l'instant `now` (FILETIME).
    pub fn position(&self, now: i64) -> Option<(f64, f64)> {
        let t = self.timeline?;
        let elapsed = if self.playing { (now - t.updated).max(0) as f64 / 1e7 } else { 0.0 };
        Some(((t.position + elapsed).clamp(0.0, t.duration), t.duration))
    }
}

/// Heure actuelle au format FILETIME, pour `NowPlaying::progress`.
pub fn filetime_now() -> i64 {
    let ft = unsafe { windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime() };
    ((ft.dwHighDateTime as i64) << 32) | ft.dwLowDateTime as i64
}

enum Command {
    TogglePlayPause,
    Previous,
    Next,
}

pub struct Media {
    state: Arc<Mutex<Option<NowPlaying>>>,
    commands: mpsc::Sender<Command>,
}

impl Media {
    /// Démarre le suivi ; `message` est posté à `notify` à chaque changement.
    pub fn start(notify: HWND, message: u32) -> Self {
        let state = Arc::new(Mutex::new(None));
        let (commands, rx) = mpsc::channel();
        let shared = Arc::clone(&state);
        let notify = notify.0 as isize;
        thread::spawn(move || worker(shared, rx, notify, message));
        Self { state, commands }
    }

    pub fn now_playing(&self) -> Option<NowPlaying> {
        self.state.lock().ok()?.clone()
    }

    pub fn toggle_play_pause(&self) {
        let _ = self.commands.send(Command::TogglePlayPause);
    }

    pub fn previous(&self) {
        let _ = self.commands.send(Command::Previous);
    }

    pub fn next(&self) {
        let _ = self.commands.send(Command::Next);
    }
}

fn worker(state: Arc<Mutex<Option<NowPlaying>>>, commands: mpsc::Receiver<Command>, notify: isize, message: u32) {
    unsafe {
        let _ = RoInitialize(RO_INIT_MULTITHREADED);
    }
    let Ok(manager) = GlobalSystemMediaTransportControlsSessionManager::RequestAsync().and_then(|op| op.get()) else {
        return;
    };
    let mut track = (String::new(), String::new());
    let mut art: Option<Arc<Image>> = None;
    loop {
        // Une lecture par seconde suffit ; une commande réveille le fil tout de suite.
        match commands.recv_timeout(Duration::from_millis(1000)) {
            Ok(command) => {
                if let Ok(session) = manager.GetCurrentSession() {
                    let _ = match command {
                        Command::TogglePlayPause => session.TryTogglePlayPauseAsync(),
                        Command::Previous => session.TrySkipPreviousAsync(),
                        Command::Next => session.TrySkipNextAsync(),
                    }
                    .and_then(|op| op.get());
                }
            }
            Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => {}
        }
        let current = read(&manager, &mut track, &mut art);
        let Ok(mut shared) = state.lock() else { return };
        let changed = match (&*shared, &current) {
            (Some(a), Some(b)) => !a.same_as(b),
            (None, None) => false,
            _ => true,
        };
        if changed {
            *shared = current;
            drop(shared);
            unsafe {
                let _ = PostMessageW(HWND(notify as _), message, WPARAM(0), LPARAM(0));
            }
        }
    }
}

fn read(
    manager: &GlobalSystemMediaTransportControlsSessionManager,
    track: &mut (String, String),
    art: &mut Option<Arc<Image>>,
) -> Option<NowPlaying> {
    let session = manager.GetCurrentSession().ok()?;
    let props = session.TryGetMediaPropertiesAsync().ok()?.get().ok()?;
    let title = props.Title().ok()?.to_string();
    if title.trim().is_empty() {
        return None;
    }
    let artist = props.Artist().map(|a| a.to_string()).unwrap_or_default();
    let info = session.GetPlaybackInfo().ok();
    let playing = info
        .as_ref()
        .and_then(|i| i.PlaybackStatus().ok())
        .is_some_and(|status| status == GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing);
    let controls = info.as_ref().and_then(|i| i.Controls().ok());
    let can_previous = controls.as_ref().is_some_and(|c| c.IsPreviousEnabled().unwrap_or(false));
    let can_next = controls.as_ref().is_some_and(|c| c.IsNextEnabled().unwrap_or(false));
    let timeline = session.GetTimelineProperties().ok().and_then(|t| {
        let duration = (t.EndTime().ok()?.Duration - t.StartTime().ok()?.Duration) as f64 / 1e7;
        let position = t.Position().ok()?.Duration as f64 / 1e7;
        let updated = t.LastUpdatedTime().ok()?.UniversalTime;
        (duration > 0.0).then_some(Timeline { position, duration, updated })
    });
    if (title.as_str(), artist.as_str()) != (track.0.as_str(), track.1.as_str()) {
        *track = (title.clone(), artist.clone());
        *art = None;
    }
    // La pochette arrive parfois un peu après le titre : on réessaie tant qu'on ne l'a pas.
    if art.is_none() {
        *art = props.Thumbnail().ok().and_then(|t| decode(&t)).map(Arc::new);
    }
    Some(NowPlaying { title, artist, playing, art: art.clone(), can_previous, can_next, timeline })
}

/// Décode la pochette en carré (on garde le centre : les vignettes vidéo sont en 16/9).
fn decode(reference: &IRandomAccessStreamReference) -> Option<Image> {
    let stream: IRandomAccessStream = reference.OpenReadAsync().ok()?.get().ok()?.cast().ok()?;
    let decoder = BitmapDecoder::CreateAsync(&stream).ok()?.get().ok()?;
    let (w, h) = (decoder.PixelWidth().ok()?, decoder.PixelHeight().ok()?);
    let side = w.min(h).max(1);
    let (sw, sh) = ((w * ART_SIZE / side).max(ART_SIZE), (h * ART_SIZE / side).max(ART_SIZE));
    let transform = BitmapTransform::new().ok()?;
    transform.SetScaledWidth(sw).ok()?;
    transform.SetScaledHeight(sh).ok()?;
    transform.SetInterpolationMode(BitmapInterpolationMode::Fant).ok()?;
    transform
        .SetBounds(BitmapBounds { X: (sw - ART_SIZE) / 2, Y: (sh - ART_SIZE) / 2, Width: ART_SIZE, Height: ART_SIZE })
        .ok()?;
    let data = decoder
        .GetPixelDataTransformedAsync(
            BitmapPixelFormat::Bgra8,
            BitmapAlphaMode::Premultiplied,
            &transform,
            ExifOrientationMode::RespectExifOrientation,
            ColorManagementMode::DoNotColorManage,
        )
        .ok()?
        .get()
        .ok()?;
    let bytes = data.DetachPixelData().ok()?;
    let px: Vec<u32> = bytes.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
    if px.len() != (ART_SIZE * ART_SIZE) as usize {
        return None;
    }
    let mut image = Image { w: ART_SIZE as i32, h: ART_SIZE as i32, px };
    image.round_corners(ART_SIZE as f32 / 10.0);
    Some(image)
}
