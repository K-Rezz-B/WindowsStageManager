//! Fonctionnalités activées ou non, choisies dans la fenêtre des réglages et enregistrées dans
//! `%APPDATA%\StageManager\settings.txt` (une ligne `nom=0` ou `nom=1` par fonctionnalité).

use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Feature {
    Scenes,
    Dock,
    Notch,
    Pin,
    Float,
    Launcher,
    Corners,
}

impl Feature {
    /// Dans l'ordre de la fenêtre des réglages.
    pub const ALL: [Feature; 7] =
        [Feature::Scenes, Feature::Dock, Feature::Notch, Feature::Pin, Feature::Float, Feature::Launcher, Feature::Corners];

    /// Nom dans le fichier des réglages.
    fn key(self) -> &'static str {
        match self {
            Feature::Scenes => "scenes",
            Feature::Dock => "dock",
            Feature::Notch => "notch",
            Feature::Pin => "pin",
            Feature::Float => "float",
            Feature::Launcher => "launcher",
            Feature::Corners => "corners",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Feature::Scenes => "Stage Manager",
            Feature::Dock => "Barre d'applis",
            Feature::Notch => "Encoche",
            Feature::Pin => "Page accrochée à l'encoche",
            Feature::Float => "Fenêtres épinglées",
            Feature::Launcher => "Lanceur",
            Feature::Corners => "Coins arrondis",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Feature::Scenes => "Les fenêtres rangées en scènes, à droite de l'encoche · Ctrl+Alt+S",
            Feature::Dock => "Applis, corbeille, volume et lanceur à gauche de l'encoche, à la place du Dock et de la barre des tâches",
            Feature::Notch => "L'heure et le lecteur de musique en haut de l'écran",
            Feature::Pin => "Une page web en direct dans l'encoche · Ctrl+Alt+P",
            Feature::Float => "Glisser une fenêtre sur l'encoche la garde au premier plan",
            Feature::Launcher => "Rechercher et lancer une appli · Alt+Espace",
            Feature::Corners => "Les coins de l'écran arrondis, comme sur les Mac",
        }
    }

    /// Fonctionnalité dont celle-ci a besoin pour servir : la page accrochée vit dans l'encoche,
    /// et c'est sur l'encoche qu'on dépose une fenêtre pour l'épingler.
    pub fn parent(self) -> Option<Feature> {
        matches!(self, Feature::Pin | Feature::Float).then_some(Feature::Notch)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Features {
    pub scenes: bool,
    pub dock: bool,
    pub notch: bool,
    pub pin: bool,
    pub float: bool,
    pub launcher: bool,
    pub corners: bool,
}

impl Default for Features {
    /// Tout est activé, comme avant l'arrivée des réglages.
    fn default() -> Self {
        Self { scenes: true, dock: true, notch: true, pin: true, float: true, launcher: true, corners: true }
    }
}

impl Features {
    pub const NONE: Features = Features { scenes: false, dock: false, notch: false, pin: false, float: false, launcher: false, corners: false };

    pub fn get(&self, f: Feature) -> bool {
        match f {
            Feature::Scenes => self.scenes,
            Feature::Dock => self.dock,
            Feature::Notch => self.notch,
            Feature::Pin => self.pin,
            Feature::Float => self.float,
            Feature::Launcher => self.launcher,
            Feature::Corners => self.corners,
        }
    }

    pub fn set(&mut self, f: Feature, on: bool) {
        let slot = match f {
            Feature::Scenes => &mut self.scenes,
            Feature::Dock => &mut self.dock,
            Feature::Notch => &mut self.notch,
            Feature::Pin => &mut self.pin,
            Feature::Float => &mut self.float,
            Feature::Launcher => &mut self.launcher,
            Feature::Corners => &mut self.corners,
        };
        *slot = on;
    }

    /// La fonctionnalité sert-elle vraiment ? (activée, et celle dont elle dépend aussi)
    pub fn active(&self, f: Feature) -> bool {
        self.get(f) && f.parent().is_none_or(|p| self.get(p))
    }

    /// Lit le fichier ; ce qu'il ne mentionne pas reste activé.
    pub fn parse(text: &str) -> Features {
        let mut features = Features::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else { continue };
            if let Some(f) = Feature::ALL.into_iter().find(|f| f.key() == key.trim()) {
                features.set(f, value.trim() != "0");
            }
        }
        features
    }

    pub fn format(&self) -> String {
        Feature::ALL.iter().map(|&f| format!("{}={}\n", f.key(), self.get(f) as u8)).collect()
    }
}

fn file() -> Option<PathBuf> {
    Some(Path::new(&std::env::var("APPDATA").ok()?).join("StageManager").join("settings.txt"))
}

pub fn load() -> Features {
    file().and_then(|f| std::fs::read_to_string(f).ok()).map_or_else(Features::default, |text| Features::parse(&text))
}

pub fn save(features: &Features) {
    let Some(file) = file() else { return };
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(file, features.format());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aller_retour() {
        let mut f = Features::default();
        f.set(Feature::Dock, false);
        f.set(Feature::Corners, false);
        assert_eq!(Features::parse(&f.format()), f);
    }

    #[test]
    fn fichier_incomplet() {
        // Une fonctionnalité absente du fichier (ajoutée depuis) reste activée.
        let f = Features::parse("dock=0\n  notch = 0 \nbidule=0\n");
        assert!(!f.dock && !f.notch);
        assert!(f.scenes && f.pin && f.float && f.launcher && f.corners);
    }

    #[test]
    fn page_sans_encoche() {
        let mut f = Features::default();
        assert!(f.active(Feature::Pin));
        f.set(Feature::Notch, false);
        assert!(f.get(Feature::Pin) && !f.active(Feature::Pin));
        assert!(f.get(Feature::Float) && !f.active(Feature::Float), "on dépose les fenêtres sur l'encoche");
        assert!(f.active(Feature::Dock));
    }
}
