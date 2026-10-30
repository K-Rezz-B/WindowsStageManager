//! Modèle des scènes, sans dépendance à Win32 pour pouvoir être testé.

use std::collections::HashMap;

/// Identifiant d'une fenêtre (valeur brute du HWND).
pub type Wid = isize;

#[derive(Debug)]
pub struct Stage {
    pub id: u64,
    /// Fenêtres de la scène, la plus récemment utilisée en premier.
    pub windows: Vec<Wid>,
    last_used: u64,
}

/// Ce qu'il faut faire subir aux fenêtres après un changement de scène.
#[derive(Debug, Default, PartialEq)]
pub struct Transition {
    pub minimize: Vec<Wid>,
    /// À restaurer sans les activer, de l'arrière vers l'avant.
    pub restore: Vec<Wid>,
    /// À restaurer et passer au premier plan, en dernier.
    pub focus: Option<Wid>,
}

impl Transition {
    pub fn is_empty(&self) -> bool {
        self.minimize.is_empty() && self.restore.is_empty() && self.focus.is_none()
    }
}

#[derive(Debug, Default)]
pub struct Model {
    stages: Vec<Stage>,
    /// Application (chemin de l'exécutable) de chaque fenêtre suivie.
    apps: HashMap<Wid, String>,
    active: Option<u64>,
    next_id: u64,
    clock: u64,
}

impl Model {
    /// Repart de zéro avec une scène par application.
    /// `windows` est dans l'ordre Z : la première est au premier plan.
    pub fn init(&mut self, windows: &[(Wid, String)], foreground: Option<Wid>) -> Transition {
        *self = Self::default();
        let mut by_app: Vec<(&str, Vec<Wid>)> = Vec::new();
        for (w, app) in windows {
            self.apps.insert(*w, app.clone());
            match by_app.iter_mut().find(|(a, _)| *a == app.as_str()) {
                Some((_, ws)) => ws.push(*w),
                None => by_app.push((app, vec![*w])),
            }
        }
        // Créées de la plus basse à la plus haute : la plus haute est la plus récente.
        for (_, ws) in by_app.into_iter().rev() {
            self.push_stage(ws);
        }

        let active = foreground.and_then(|w| self.stage_of(w)).or_else(|| self.most_recent());
        let Some(active) = active else {
            return Transition::default();
        };
        if let (Some(w), Some(i)) = (foreground, self.index(active)) {
            bring_to_front(&mut self.stages[i].windows, w);
        }
        self.touch(active);
        self.active = Some(active);
        let minimize = self
            .stages
            .iter()
            .filter(|s| s.id != active)
            .flat_map(|s| s.windows.iter().copied())
            .collect();
        Transition { minimize, ..Default::default() }
    }

    #[cfg(test)]
    pub fn active(&self) -> Option<u64> {
        self.active
    }

    /// Scène à afficher quand la scène active vient de se vider : la plus récente de la barre.
    pub fn next_stage(&self) -> Option<u64> {
        if self.active.is_some() {
            return None;
        }
        self.sidebar().first().map(|s| s.id)
    }

    pub fn stage(&self, id: u64) -> Option<&Stage> {
        self.stages.iter().find(|s| s.id == id)
    }

    pub fn is_tracked(&self, w: Wid) -> bool {
        self.apps.contains_key(&w)
    }

    pub fn stage_of(&self, w: Wid) -> Option<u64> {
        self.stages.iter().find(|s| s.windows.contains(&w)).map(|s| s.id)
    }

    pub fn in_active_stage(&self, w: Wid) -> bool {
        self.active.is_some() && self.stage_of(w) == self.active
    }

    pub fn windows(&self) -> Vec<Wid> {
        self.apps.keys().copied().collect()
    }

    /// Scènes affichées dans la barre latérale (toutes sauf l'active), la plus récente en premier.
    pub fn sidebar(&self) -> Vec<&Stage> {
        let mut stages: Vec<&Stage> = self.stages.iter().filter(|s| Some(s.id) != self.active).collect();
        stages.sort_by(|a, b| b.last_used.cmp(&a.last_used));
        stages
    }

    /// Fait de `id` la scène active, avec `focus` au premier plan si elle en fait partie.
    pub fn activate(&mut self, id: u64, focus: Option<Wid>) -> Transition {
        let Some(idx) = self.index(id) else {
            return Transition::default();
        };
        if let Some(w) = focus {
            bring_to_front(&mut self.stages[idx].windows, w);
        }
        self.touch(id);
        if self.active == Some(id) {
            return Transition::default();
        }
        let minimize = self
            .active
            .and_then(|a| self.index(a))
            .map(|i| self.stages[i].windows.clone())
            .unwrap_or_default();
        self.active = Some(id);
        let windows = &self.stages[idx].windows;
        Transition {
            minimize,
            restore: windows[1..].iter().rev().copied().collect(),
            focus: windows.first().copied(),
        }
    }

    /// Une fenêtre a pris le focus. Si elle est nouvelle, elle rejoint la scène active
    /// quand celle-ci contient déjà son application, sinon elle ouvre sa propre scène.
    pub fn add_focused(&mut self, w: Wid, app: &str) -> Transition {
        if let Some(id) = self.stage_of(w) {
            return self.activate(id, Some(w));
        }
        self.apps.insert(w, app.to_owned());
        if let Some(i) = self.active.and_then(|a| self.index(a)) {
            let same_app = self.stages[i].windows.iter().any(|x| self.apps.get(x).is_some_and(|a| a == app));
            if same_app {
                self.stages[i].windows.insert(0, w);
                return Transition::default();
            }
        }
        let id = self.push_stage(vec![w]);
        self.activate(id, Some(w))
    }

    /// Ajoute à la scène active une fenêtre apparue sans prendre le focus.
    pub fn adopt(&mut self, w: Wid, app: &str) {
        if self.is_tracked(w) {
            return;
        }
        self.apps.insert(w, app.to_owned());
        match self.active.and_then(|a| self.index(a)) {
            Some(i) => self.stages[i].windows.push(w),
            None => {
                let id = self.push_stage(vec![w]);
                self.active = Some(id);
            }
        }
    }

    /// Déplace toutes les fenêtres de la scène `id` dans la scène active.
    pub fn merge_into_active(&mut self, id: u64) -> Transition {
        let Some(active) = self.active.filter(|a| *a != id && self.index(*a).is_some()) else {
            return self.activate(id, None);
        };
        let Some(idx) = self.index(id) else {
            return Transition::default();
        };
        let moved = self.stages.remove(idx).windows;
        let a = self.index(active).expect("la scène active existe");
        self.stages[a].windows.splice(0..0, moved.iter().copied());
        self.touch(active);
        Transition {
            minimize: Vec::new(),
            restore: moved[1..].iter().rev().copied().collect(),
            focus: moved.first().copied(),
        }
    }

    /// Peut-on sortir cette fenêtre de la scène active ? (il faut qu'il lui en reste une autre)
    pub fn can_detach(&self, w: Wid) -> bool {
        self.active
            .and_then(|a| self.stage(a))
            .is_some_and(|s| s.windows.len() > 1 && s.windows.contains(&w))
    }

    /// Sort une fenêtre de la scène active pour en faire une scène à part, rangée en tête de barre.
    pub fn detach(&mut self, w: Wid) -> Transition {
        if !self.can_detach(w) {
            return Transition::default();
        }
        let Some(i) = self.active.and_then(|a| self.index(a)) else {
            return Transition::default();
        };
        self.stages[i].windows.retain(|x| *x != w);
        let focus = self.stages[i].windows.first().copied();
        self.push_stage(vec![w]);
        Transition { minimize: vec![w], restore: Vec::new(), focus }
    }

    /// Éclate une scène de la barre en une scène par fenêtre, à la même place dans la barre.
    pub fn split(&mut self, id: u64) -> bool {
        let Some(i) = self.index(id).filter(|_| self.active != Some(id)) else {
            return false;
        };
        if self.stages[i].windows.len() < 2 {
            return false;
        }
        let last_used = self.stages[i].last_used;
        for w in self.stages[i].windows.split_off(1) {
            let id = self.push_stage(vec![w]);
            // Même ancienneté : le tri stable les garde juste après la scène d'origine.
            if let Some(j) = self.index(id) {
                self.stages[j].last_used = last_used;
            }
        }
        true
    }

    /// Oublie une fenêtre (fermée, cachée ou réduite par l'utilisateur).
    pub fn remove(&mut self, w: Wid) -> bool {
        if self.apps.remove(&w).is_none() {
            return false;
        }
        for s in &mut self.stages {
            s.windows.retain(|x| *x != w);
        }
        self.stages.retain(|s| !s.windows.is_empty());
        if self.active.is_some_and(|a| self.index(a).is_none()) {
            self.active = None;
        }
        true
    }

    fn index(&self, id: u64) -> Option<usize> {
        self.stages.iter().position(|s| s.id == id)
    }

    fn most_recent(&self) -> Option<u64> {
        self.stages.iter().max_by_key(|s| s.last_used).map(|s| s.id)
    }

    fn touch(&mut self, id: u64) {
        self.clock += 1;
        if let Some(i) = self.index(id) {
            self.stages[i].last_used = self.clock;
        }
    }

    fn push_stage(&mut self, windows: Vec<Wid>) -> u64 {
        self.next_id += 1;
        self.clock += 1;
        self.stages.push(Stage { id: self.next_id, windows, last_used: self.clock });
        self.next_id
    }
}

fn bring_to_front(windows: &mut Vec<Wid>, w: Wid) {
    if let Some(p) = windows.iter().position(|x| *x == w) {
        let w = windows.remove(p);
        windows.insert(0, w);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ordre Z : 1 (edge), 2 (code, au premier plan), 3 (edge), 4 (explorer).
    fn model() -> (Model, Transition) {
        let mut m = Model::default();
        let windows = [(1, "edge"), (2, "code"), (3, "edge"), (4, "explorer")].map(|(w, a)| (w, a.to_string()));
        let t = m.init(&windows, Some(2));
        (m, t)
    }

    fn active_windows(m: &Model) -> Vec<Wid> {
        m.stage(m.active().unwrap()).unwrap().windows.clone()
    }

    fn sidebar(m: &Model) -> Vec<Vec<Wid>> {
        m.sidebar().iter().map(|s| s.windows.clone()).collect()
    }

    #[test]
    fn init_groupe_par_application() {
        let (m, t) = model();
        assert_eq!(active_windows(&m), vec![2]);
        let mut minimized = t.minimize.clone();
        minimized.sort();
        assert_eq!(minimized, vec![1, 3, 4]);
        assert_eq!(sidebar(&m), vec![vec![1, 3], vec![4]]);
    }

    #[test]
    fn changer_de_scene() {
        let (mut m, _) = model();
        let edge = m.stage_of(3).unwrap();
        let t = m.activate(edge, Some(3));
        assert_eq!(t, Transition { minimize: vec![2], restore: vec![1], focus: Some(3) });
        assert_eq!(sidebar(&m)[0], vec![2]);
        assert!(m.activate(edge, Some(1)).is_empty());
        assert_eq!(active_windows(&m), vec![1, 3]);
    }

    #[test]
    fn nouvelle_fenetre_de_la_meme_appli_rejoint_la_scene() {
        let (mut m, _) = model();
        assert!(m.add_focused(5, "code").is_empty());
        assert_eq!(active_windows(&m), vec![5, 2]);
    }

    #[test]
    fn nouvelle_appli_ouvre_une_scene() {
        let (mut m, _) = model();
        let t = m.add_focused(6, "notepad");
        assert_eq!(t, Transition { minimize: vec![2], restore: vec![], focus: Some(6) });
        assert_eq!(sidebar(&m)[0], vec![2]);
    }

    #[test]
    fn fermer_la_derniere_fenetre_supprime_la_scene() {
        let (mut m, _) = model();
        assert!(m.remove(4));
        assert_eq!(m.sidebar().len(), 1);
        assert!(m.remove(2));
        assert_eq!(m.active(), None);
        assert!(!m.remove(2));
    }

    #[test]
    fn la_scene_suivante_remplace_une_scene_videe() {
        let (mut m, _) = model();
        assert_eq!(m.next_stage(), None, "la scène active n'est pas vide");
        // On ferme la seule fenêtre de la scène active (code) : edge, la plus récente, prend la place.
        m.remove(2);
        let edge = m.stage_of(1).unwrap();
        assert_eq!(m.next_stage(), Some(edge));
        let t = m.activate(edge, None);
        assert_eq!(t, Transition { minimize: vec![], restore: vec![3], focus: Some(1) });
        assert_eq!(m.next_stage(), None);
    }

    #[test]
    fn fusion_dans_la_scene_active() {
        let (mut m, _) = model();
        let edge = m.stage_of(1).unwrap();
        let t = m.merge_into_active(edge);
        assert_eq!(t, Transition { minimize: vec![], restore: vec![3], focus: Some(1) });
        assert_eq!(active_windows(&m), vec![1, 3, 2]);
        assert_eq!(m.sidebar().len(), 1);
    }

    #[test]
    fn detacher_une_fenetre_de_la_scene_active() {
        let (mut m, _) = model();
        let edge = m.stage_of(1).unwrap();
        m.activate(edge, Some(1));
        let t = m.detach(1);
        assert_eq!(t, Transition { minimize: vec![1], restore: vec![], focus: Some(3) });
        assert_eq!(active_windows(&m), vec![3]);
        assert_eq!(sidebar(&m)[0], vec![1], "la fenêtre détachée arrive en tête de barre");
        // La dernière fenêtre d'une scène ne se détache pas.
        assert!(m.detach(3).is_empty());
        assert!(!m.can_detach(3));
    }

    #[test]
    fn eclater_une_scene_de_la_barre() {
        let (mut m, _) = model();
        let edge = m.stage_of(1).unwrap();
        assert!(m.split(edge));
        assert_eq!(sidebar(&m), vec![vec![1], vec![3], vec![4]]);
        // La scène active ne s'éclate pas depuis la barre.
        assert!(!m.split(m.active().unwrap()));
    }

    #[test]
    fn adoption_sans_scene_active() {
        let mut m = Model::default();
        m.adopt(7, "paint");
        assert_eq!(active_windows(&m), vec![7]);
        m.adopt(8, "word");
        assert_eq!(active_windows(&m), vec![7, 8]);
    }
}
