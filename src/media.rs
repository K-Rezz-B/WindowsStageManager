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
}

impl NowPlaying {
    fn same_as(&self, other: &NowPlaying) -> bool {
        self.title == other.title
            && self.artist == other.artist
            && self.playing == other.playing
            && self.art.is_some() == other.art.is_some()
    }
}

enum Command {
    TogglePlayPause,
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
            Ok(Command::TogglePlayPause) => {
                if let Ok(session) = manager.GetCurrentSession() {
                    let _ = session.TryTogglePlayPauseAsync().and_then(|op| op.get());
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
    let playing = session
        .GetPlaybackInfo()
        .and_then(|info| info.PlaybackStatus())
        .is_ok_and(|status| status == GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing);
    if (title.as_str(), artist.as_str()) != (track.0.as_str(), track.1.as_str()) {
        *track = (title.clone(), artist.clone());
        *art = None;
    }
    // La pochette arrive parfois un peu après le titre : on réessaie tant qu'on ne l'a pas.
    if art.is_none() {
        *art = props.Thumbnail().ok().and_then(|t| decode(&t)).map(Arc::new);
    }
    Some(NowPlaying { title, artist, playing, art: art.clone() })
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
