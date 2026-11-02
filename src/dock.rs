//! Les applis de la barre d'applis : épinglées puis ouvertes, puis la corbeille. Ce module ne
//! contient que le modèle (testable) ; la fenêtre et le dessin sont dans `app/dockbar.rs`.

use std::{
    path::Path,
    rc::Rc,
    time::{Duration, Instant},
};

use crate::{canvas::Image, shell};

/// Le rebond s'arrête quand l'appli a ouvert une fenêtre, ou au bout de ce délai.
const BOUNCE_LIMIT: Duration = Duration::from_secs(8);

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    App,
    Trash,
}

pub struct Item {
    pub kind: Kind,
    /// Raccourci (.lnk) ou exécutable à lancer.
    pub path: String,
    /// Exécutable visé, en minuscules : sert à reconnaître l'appli parmi les fenêtres ouvertes.
    pub target: String,
    pub name: String,
    pub icon: Option<Rc<Image>>,
    pub pinned: bool,
    pub running: bool,
    /// Lancée depuis le Dock et pas encore ouverte : l'icône rebondit.
    pub launched: Option<Instant>,
}

impl Item {
    fn new(kind: Kind, path: &str, pinned: bool) -> Self {
        let lower = path.to_lowercase();
        let stem = Path::new(path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        Self {
            kind,
            path: path.to_owned(),
            target: if lower.ends_with(".exe") { lower } else { String::new() },
            name: if kind == Kind::Trash { "Corbeille".to_owned() } else { stem },
            icon: None,
            pinned,
            running: false,
            launched: None,
        }
    }

    /// Cette appli est-elle celle de l'exécutable `exe` (chemin en minuscules) ?
    /// Certains raccourcis visent un lanceur (Discord passe par Update.exe) : on accepte aussi
    /// un exécutable rangé sous le dossier du lanceur et dont le nom rappelle celui de l'appli.
    pub fn matches(&self, exe: &str) -> bool {
        if self.kind != Kind::App || self.target.is_empty() {
            return false;
        }
        if self.target == exe {
            return true;
        }
        let Some(dir) = Path::new(&self.target).parent().map(|d| d.to_string_lossy().into_owned()) else { return false };
        if !exe.starts_with(&format!("{dir}\\")) {
            return false;
        }
        let exe_stem = Path::new(exe).file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
        let name: String = self.name.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect();
        !name.is_empty() && (exe_stem.contains(&name) || name.contains(&exe_stem))
    }
}

pub struct Dock {
    pub items: Vec<Item>,
}

impl Dock {
    /// `pins` : raccourcis ou exécutables épinglés, dans l'ordre.
    pub fn new(pins: &[String]) -> Self {
        let mut items: Vec<Item> = pins.iter().map(|p| Item::new(Kind::App, p, true)).collect();
        items.push(Item::new(Kind::Trash, shell::RECYCLE_BIN, true));
        Self { items }
    }

    pub fn pins(&self) -> Vec<String> {
        self.items.iter().filter(|i| i.kind == Kind::App && i.pinned).map(|i| i.path.clone()).collect()
    }

    fn trash_index(&self) -> usize {
        self.items.iter().position(|i| i.kind == Kind::Trash).unwrap_or(self.items.len())
    }

    /// Met à jour les applis ouvertes (exécutables en minuscules). Renvoie les nouvelles
    /// applis non épinglées, dont il faut charger l'icône et le nom.
    pub fn set_running(&mut self, running: &[String], now: Instant) -> Vec<String> {
        for item in self.items.iter_mut().filter(|i| i.kind == Kind::App) {
            item.running = running.iter().any(|exe| item.matches(exe));
            if item.running || item.launched.is_some_and(|t| now.duration_since(t) > BOUNCE_LIMIT) {
                item.launched = None;
            }
        }
        // Les applis fermées qui n'étaient pas épinglées quittent le Dock.
        self.items.retain(|i| i.kind != Kind::App || i.pinned || i.running || i.launched.is_some());
        let mut added = Vec::new();
        for exe in running {
            if !self.items.iter().any(|i| i.matches(exe)) {
                let at = self.trash_index();
                let mut item = Item::new(Kind::App, exe, false);
                item.running = true;
                self.items.insert(at, item);
                added.push(exe.clone());
            }
        }
        self.merge_duplicates();
        added
    }

    /// Une appli ouverte ajoutée au Dock avant qu'on sache la relier à son icône épinglée (la
    /// cible des raccourcis se charge en arrière-plan) fusionne avec elle. Un raccourci dont
    /// Windows ne donne pas la cible (Battle.net…) est rapproché par le nom de l'appli.
    pub fn merge_duplicates(&mut self) {
        let normalize = |s: &str| s.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect::<String>();
        let mut i = 0;
        while i < self.items.len() {
            let item = &self.items[i];
            if item.kind != Kind::App || item.pinned {
                i += 1;
                continue;
            }
            let (exe, name) = (item.target.clone(), normalize(&item.name));
            let twin = self.items.iter().position(|p| {
                p.kind == Kind::App
                    && p.pinned
                    && (p.matches(&exe) || (p.target.is_empty() && !name.is_empty() && normalize(&p.name) == name))
            });
            match twin {
                Some(p) => {
                    let pinned = &mut self.items[p];
                    if pinned.target.is_empty() {
                        pinned.target = exe;
                    }
                    pinned.running = true;
                    pinned.launched = None;
                    self.items.remove(i);
                }
                None => i += 1,
            }
        }
    }

    /// Épingle ou désépingle une appli. Une appli fermée qu'on désépingle quitte le Dock.
    pub fn toggle_pin(&mut self, index: usize) {
        let Some(item) = self.items.get_mut(index).filter(|i| i.kind == Kind::App) else { return };
        item.pinned = !item.pinned;
        if !item.pinned && !item.running {
            self.items.remove(index);
        } else if item.pinned {
            // Les épinglées passent avant les applis simplement ouvertes.
            let item = self.items.remove(index);
            let at = self.items.iter().position(|i| i.kind != Kind::App || !i.pinned).unwrap_or(self.items.len());
            self.items.insert(at, item);
        }
    }

    /// Une appli lancée attend sa fenêtre : son icône sautille.
    pub fn animating(&self) -> bool {
        self.items.iter().any(|i| i.launched.is_some())
    }
}

/// Fichier des applis épinglées (un chemin par ligne), dans les données de l'utilisateur.
fn pins_file() -> Option<std::path::PathBuf> {
    Some(Path::new(&std::env::var("APPDATA").ok()?).join("StageManager").join("dock.txt"))
}

/// Applis épinglées enregistrées ; au tout premier lancement, celles de la barre des tâches.
pub fn load_pins() -> Vec<String> {
    match pins_file().and_then(|f| std::fs::read_to_string(f).ok()) {
        Some(text) => text.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_owned).collect(),
        None => shell::taskbar_pins(),
    }
}

pub fn save_pins(pins: &[String]) {
    let Some(file) = pins_file() else { return };
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(file, pins.join("\n"));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dock() -> Dock {
        Dock::new(&["C:\\Apps\\Discord.lnk".into(), "C:\\Program Files\\Zen\\zen.exe".into()])
    }

    #[test]
    fn reconnait_une_appli_derriere_son_lanceur() {
        let mut item = Item::new(Kind::App, "C:\\Apps\\Discord.lnk", true);
        item.target = "c:\\users\\k\\appdata\\local\\discord\\update.exe".into();
        assert!(item.matches("c:\\users\\k\\appdata\\local\\discord\\app-1.0.9\\discord.exe"));
        assert!(!item.matches("c:\\users\\k\\appdata\\local\\autre\\discord.exe"));
        let zen = Item::new(Kind::App, "C:\\Program Files\\Zen\\zen.exe", true);
        assert!(zen.matches("c:\\program files\\zen\\zen.exe"));
    }

    #[test]
    fn applis_ouvertes_ajoutees_avant_la_corbeille_puis_retirees() {
        let mut d = dock();
        let now = Instant::now();
        let added = d.set_running(&["c:\\program files\\zen\\zen.exe".into(), "c:\\jeux\\steam.exe".into()], now);
        assert_eq!(added, vec!["c:\\jeux\\steam.exe".to_string()]);
        let kinds: Vec<Kind> = d.items.iter().map(|i| i.kind).collect();
        assert_eq!(kinds, vec![Kind::App, Kind::App, Kind::App, Kind::Trash]);
        assert!(d.items[1].running && d.items[2].running && !d.items[0].running);
        // Steam se ferme : non épinglée, elle quitte le Dock.
        d.set_running(&["c:\\program files\\zen\\zen.exe".into()], now);
        assert_eq!(d.items.len(), 3);
    }

    #[test]
    fn une_appli_ouverte_rejoint_son_icone_epinglee() {
        let mut d = Dock::new(&["C:\\Pins\\Battle\u{2024}net.lnk".into(), "C:\\Pins\\Zen.lnk".into()]);
        let now = Instant::now();
        // Les cibles des raccourcis ne sont pas encore connues : les applis s'ajoutent à part.
        d.set_running(&["c:\\apps\\battle.net\\battle.net.exe".into(), "c:\\zen\\zen.exe".into()], now);
        assert_eq!(d.items.len(), 3, "Battle.net, sans cible, est reconnu par son nom");
        // La cible de Zen arrive : le doublon disparaît.
        d.items[1].target = "c:\\zen\\zen.exe".into();
        d.merge_duplicates();
        assert_eq!(d.items.len(), 3);
        assert!(d.items[0].running && d.items[1].running);
        assert_eq!(d.items[0].target, "c:\\apps\\battle.net\\battle.net.exe");
    }

    #[test]
    fn epingler_et_desepingler() {
        let mut d = dock();
        let now = Instant::now();
        d.set_running(&["c:\\jeux\\steam.exe".into()], now);
        d.toggle_pin(2);
        assert_eq!(d.pins().len(), 3);
        d.toggle_pin(0);
        assert_eq!(d.items.len(), 3, "Discord, fermée et désépinglée, disparaît");
    }
}
