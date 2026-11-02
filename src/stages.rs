//! Modèle des scènes, sans dépendance à Win32 pour pouvoir être testé.
//!
//! Chaque écran a ses propres scènes et sa propre scène active, comme sur macOS : changer de
//! scène sur un écran ne touche pas aux autres.

use std::collections::HashMap;

/// Identifiant d'une fenêtre (valeur brute du HWND).
pub type Wid = isize;
/// Identifiant d'un écran.
pub type ScreenId = u32;

#[derive(Debug)]
pub struct Stage {
    pub id: u64,
    /// Écran où vit la scène.
    pub screen: ScreenId,
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
    /// Scène active de chaque écran.
    active: HashMap<ScreenId, u64>,
    next_id: u64,
    clock: u64,
}

impl Model {
    /// Repart de zéro avec une scène par application et par écran.
    /// `windows` est dans l'ordre Z : la première est au premier plan.
    pub fn init(&mut self, windows: &[(Wid, String, ScreenId)], foreground: Option<Wid>) -> Transition {
        *self = Self::default();
        let mut groups: Vec<(ScreenId, &str, Vec<Wid>)> = Vec::new();
        for (w, app, screen) in windows {
            self.apps.insert(*w, app.clone());
            match groups.iter_mut().find(|(s, a, _)| s == screen && *a == app.as_str()) {
                Some((_, _, ws)) => ws.push(*w),
                None => groups.push((*screen, app, vec![*w])),
            }
        }
        // Créées de la plus basse à la plus haute : la plus haute est la plus récente.
        for (screen, _, ws) in groups.into_iter().rev() {
            self.push_stage(screen, ws);
        }

        let mut screens: Vec<ScreenId> = self.stages.iter().map(|s| s.screen).collect();
        screens.sort_unstable();
        screens.dedup();
        let focused = foreground.and_then(|w| self.stage_of(w));
        let mut minimize = Vec::new();
        for screen in screens {
            // La scène de la fenêtre au premier plan, sinon la plus récente de l'écran.
            let active = focused
                .filter(|id| self.stage(*id).is_some_and(|s| s.screen == screen))
                .or_else(|| self.most_recent(screen));
            let Some(active) = active else { continue };
            if let (Some(w), Some(i)) = (foreground, self.index(active)) {
                bring_to_front(&mut self.stages[i].windows, w);
            }
            self.touch(active);
            self.active.insert(screen, active);
            minimize.extend(
                self.stages
                    .iter()
                    .filter(|s| s.screen == screen && s.id != active)
                    .flat_map(|s| s.windows.iter().copied()),
            );
        }
        Transition { minimize, ..Default::default() }
    }

    /// Scène active d'un écran.
    pub fn active(&self, screen: ScreenId) -> Option<u64> {
        self.active.get(&screen).copied()
    }

    /// Scène à afficher quand la scène active d'un écran vient de se vider : la plus récente
    /// de sa barre.
    pub fn next_stage(&self, screen: ScreenId) -> Option<u64> {
        if self.active.contains_key(&screen) {
            return None;
        }
        self.sidebar(screen).first().map(|s| s.id)
    }

    pub fn stage(&self, id: u64) -> Option<&Stage> {
        self.stages.iter().find(|s| s.id == id)
    }

    pub fn is_tracked(&self, w: Wid) -> bool {
        self.apps.contains_key(&w)
    }

    fn stage_with(&self, w: Wid) -> Option<&Stage> {
        self.stages.iter().find(|s| s.windows.contains(&w))
    }

    pub fn stage_of(&self, w: Wid) -> Option<u64> {
        self.stage_with(w).map(|s| s.id)
    }

    /// Écran de la scène d'une fenêtre.
    pub fn screen_of(&self, w: Wid) -> Option<ScreenId> {
        self.stage_with(w).map(|s| s.screen)
    }

    /// La fenêtre est-elle dans la scène active de son écran ?
    pub fn in_active_stage(&self, w: Wid) -> bool {
        self.stage_with(w).is_some_and(|s| self.active(s.screen) == Some(s.id))
    }

    pub fn windows(&self) -> Vec<Wid> {
        self.apps.keys().copied().collect()
    }

    /// Scènes affichées dans la barre latérale d'un écran (toutes les siennes sauf l'active),
    /// la plus récente en premier.
    pub fn sidebar(&self, screen: ScreenId) -> Vec<&Stage> {
        let active = self.active(screen);
        let mut stages: Vec<&Stage> = self.stages.iter().filter(|s| s.screen == screen && Some(s.id) != active).collect();
        stages.sort_by(|a, b| b.last_used.cmp(&a.last_used));
        stages
    }

    /// Fait de `id` la scène active de son écran, avec `focus` au premier plan si elle en fait
    /// partie.
    pub fn activate(&mut self, id: u64, focus: Option<Wid>) -> Transition {
        let Some(idx) = self.index(id) else {
            return Transition::default();
        };
        if let Some(w) = focus {
            bring_to_front(&mut self.stages[idx].windows, w);
        }
        self.touch(id);
        let screen = self.stages[idx].screen;
        if self.active(screen) == Some(id) {
            return Transition::default();
        }
        let minimize = self
            .active(screen)
            .and_then(|a| self.index(a))
            .map(|i| self.stages[i].windows.clone())
            .unwrap_or_default();
        self.active.insert(screen, id);
        let windows = &self.stages[idx].windows;
        Transition {
            minimize,
            restore: windows[1..].iter().rev().copied().collect(),
            focus: windows.first().copied(),
        }
    }

    /// Une fenêtre de l'écran `screen` a pris le focus. Si elle est nouvelle, elle rejoint la
    /// scène active de l'écran quand celle-ci contient déjà son application, sinon elle ouvre
    /// sa propre scène.
    pub fn add_focused(&mut self, w: Wid, app: &str, screen: ScreenId) -> Transition {
        if let Some(id) = self.stage_of(w) {
            return self.activate(id, Some(w));
        }
        self.apps.insert(w, app.to_owned());
        if let Some(i) = self.active(screen).and_then(|a| self.index(a)) {
            let same_app = self.stages[i].windows.iter().any(|x| self.apps.get(x).is_some_and(|a| a == app));
            if same_app {
                self.stages[i].windows.insert(0, w);
                return Transition::default();
            }
        }
        let id = self.push_stage(screen, vec![w]);
        self.activate(id, Some(w))
    }

    /// Ajoute à la scène active de l'écran une fenêtre apparue sans prendre le focus.
    pub fn adopt(&mut self, w: Wid, app: &str, screen: ScreenId) {
        if self.is_tracked(w) {
            return;
        }
        self.apps.insert(w, app.to_owned());
        match self.active(screen).and_then(|a| self.index(a)) {
            Some(i) => self.stages[i].windows.push(w),
            None => {
                let id = self.push_stage(screen, vec![w]);
                self.active.insert(screen, id);
            }
        }
    }

    /// Une fenêtre affichée est passée sur un autre écran : elle quitte sa scène et rejoint,
    /// au premier plan, la scène active de cet écran. Renvoie l'écran qu'elle a quitté.
    pub fn move_window(&mut self, w: Wid, screen: ScreenId) -> Option<ScreenId> {
        let from = self.screen_of(w).filter(|s| *s != screen)?;
        let app = self.apps.get(&w)?.clone();
        self.remove(w);
        self.adopt(w, &app, screen);
        if let Some(i) = self.stage_of(w).and_then(|id| self.index(id)) {
            bring_to_front(&mut self.stages[i].windows, w);
        }
        Some(from)
    }

    /// Un écran a disparu : ses scènes passent sur l'écran `to`. Sa scène active rejoint la
    /// barre de `to` (ses fenêtres sont à réduire), sauf si `to` n'en avait pas.
    pub fn move_screen(&mut self, from: ScreenId, to: ScreenId) -> Transition {
        let old = self.active.remove(&from);
        for s in self.stages.iter_mut().filter(|s| s.screen == from) {
            s.screen = to;
        }
        let Some(old) = old else {
            return Transition::default();
        };
        if !self.active.contains_key(&to) {
            self.active.insert(to, old);
            return Transition::default();
        }
        let minimize = self.stage(old).map(|s| s.windows.clone()).unwrap_or_default();
        Transition { minimize, ..Default::default() }
    }

    /// Déplace toutes les fenêtres de la scène `id` dans la scène active de son écran.
    pub fn merge_into_active(&mut self, id: u64) -> Transition {
        let Some(screen) = self.stage(id).map(|s| s.screen) else {
            return Transition::default();
        };
        let Some(active) = self.active(screen).filter(|a| *a != id && self.index(*a).is_some()) else {
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
        self.stage_with(w)
            .is_some_and(|s| self.active(s.screen) == Some(s.id) && s.windows.len() > 1)
    }

    /// Sort une fenêtre de la scène active pour en faire une scène à part, rangée en tête de la
    /// barre de son écran.
    pub fn detach(&mut self, w: Wid) -> Transition {
        if !self.can_detach(w) {
            return Transition::default();
        }
        let Some(i) = self.stage_of(w).and_then(|id| self.index(id)) else {
            return Transition::default();
        };
        let screen = self.stages[i].screen;
        self.stages[i].windows.retain(|x| *x != w);
        let focus = self.stages[i].windows.first().copied();
        self.push_stage(screen, vec![w]);
        Transition { minimize: vec![w], restore: Vec::new(), focus }
    }

    /// Éclate une scène de la barre en une scène par fenêtre, à la même place dans la barre.
    pub fn split(&mut self, id: u64) -> bool {
        let Some(i) = self.index(id).filter(|&i| self.active(self.stages[i].screen) != Some(id)) else {
            return false;
        };
        if self.stages[i].windows.len() < 2 {
            return false;
        }
        let (last_used, screen) = (self.stages[i].last_used, self.stages[i].screen);
        for w in self.stages[i].windows.split_off(1) {
            let id = self.push_stage(screen, vec![w]);
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
        let stages = &self.stages;
        self.active.retain(|_, a| stages.iter().any(|s| s.id == *a));
        true
    }

    fn index(&self, id: u64) -> Option<usize> {
        self.stages.iter().position(|s| s.id == id)
    }

    fn most_recent(&self, screen: ScreenId) -> Option<u64> {
        self.stages.iter().filter(|s| s.screen == screen).max_by_key(|s| s.last_used).map(|s| s.id)
    }

    fn touch(&mut self, id: u64) {
        self.clock += 1;
        if let Some(i) = self.index(id) {
            self.stages[i].last_used = self.clock;
        }
    }

    fn push_stage(&mut self, screen: ScreenId, windows: Vec<Wid>) -> u64 {
        self.next_id += 1;
        self.clock += 1;
        self.stages.push(Stage { id: self.next_id, screen, windows, last_used: self.clock });
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

    /// Ordre Z : 1 (edge), 2 (code, au premier plan), 3 (edge), 4 (explorer), tous sur l'écran 0.
    fn model() -> (Model, Transition) {
        let mut m = Model::default();
        let windows = [(1, "edge"), (2, "code"), (3, "edge"), (4, "explorer")].map(|(w, a)| (w, a.to_string(), 0));
        let t = m.init(&windows, Some(2));
        (m, t)
    }

    /// Deux écrans : 1 (edge) et 2 (code, au premier plan) sur l'écran 0 ; 3 (edge) et 4
    /// (explorer) sur l'écran 1.
    fn two_screens() -> (Model, Transition) {
        let mut m = Model::default();
        let windows = [(1, "edge", 0), (2, "code", 0), (3, "edge", 1), (4, "explorer", 1)].map(|(w, a, s)| (w, a.to_string(), s));
        let t = m.init(&windows, Some(2));
        (m, t)
    }

    fn active_windows(m: &Model, screen: ScreenId) -> Vec<Wid> {
        m.stage(m.active(screen).unwrap()).unwrap().windows.clone()
    }

    fn sidebar(m: &Model, screen: ScreenId) -> Vec<Vec<Wid>> {
        m.sidebar(screen).iter().map(|s| s.windows.clone()).collect()
    }

    #[test]
    fn init_groupe_par_application() {
        let (m, t) = model();
        assert_eq!(active_windows(&m, 0), vec![2]);
        let mut minimized = t.minimize.clone();
        minimized.sort();
        assert_eq!(minimized, vec![1, 3, 4]);
        assert_eq!(sidebar(&m, 0), vec![vec![1, 3], vec![4]]);
    }

    #[test]
    fn changer_de_scene() {
        let (mut m, _) = model();
        let edge = m.stage_of(3).unwrap();
        let t = m.activate(edge, Some(3));
        assert_eq!(t, Transition { minimize: vec![2], restore: vec![1], focus: Some(3) });
        assert_eq!(sidebar(&m, 0)[0], vec![2]);
        assert!(m.activate(edge, Some(1)).is_empty());
        assert_eq!(active_windows(&m, 0), vec![1, 3]);
    }

    #[test]
    fn nouvelle_fenetre_de_la_meme_appli_rejoint_la_scene() {
        let (mut m, _) = model();
        assert!(m.add_focused(5, "code", 0).is_empty());
        assert_eq!(active_windows(&m, 0), vec![5, 2]);
    }

    #[test]
    fn nouvelle_appli_ouvre_une_scene() {
        let (mut m, _) = model();
        let t = m.add_focused(6, "notepad", 0);
        assert_eq!(t, Transition { minimize: vec![2], restore: vec![], focus: Some(6) });
        assert_eq!(sidebar(&m, 0)[0], vec![2]);
    }

    #[test]
    fn fermer_la_derniere_fenetre_supprime_la_scene() {
        let (mut m, _) = model();
        assert!(m.remove(4));
        assert_eq!(m.sidebar(0).len(), 1);
        assert!(m.remove(2));
        assert_eq!(m.active(0), None);
        assert!(!m.remove(2));
    }

    #[test]
    fn la_scene_suivante_remplace_une_scene_videe() {
        let (mut m, _) = model();
        assert_eq!(m.next_stage(0), None, "la scène active n'est pas vide");
        // On ferme la seule fenêtre de la scène active (code) : edge, la plus récente, prend la place.
        m.remove(2);
        let edge = m.stage_of(1).unwrap();
        assert_eq!(m.next_stage(0), Some(edge));
        let t = m.activate(edge, None);
        assert_eq!(t, Transition { minimize: vec![], restore: vec![3], focus: Some(1) });
        assert_eq!(m.next_stage(0), None);
    }

    #[test]
    fn fusion_dans_la_scene_active() {
        let (mut m, _) = model();
        let edge = m.stage_of(1).unwrap();
        let t = m.merge_into_active(edge);
        assert_eq!(t, Transition { minimize: vec![], restore: vec![3], focus: Some(1) });
        assert_eq!(active_windows(&m, 0), vec![1, 3, 2]);
        assert_eq!(m.sidebar(0).len(), 1);
    }

    #[test]
    fn detacher_une_fenetre_de_la_scene_active() {
        let (mut m, _) = model();
        let edge = m.stage_of(1).unwrap();
        m.activate(edge, Some(1));
        let t = m.detach(1);
        assert_eq!(t, Transition { minimize: vec![1], restore: vec![], focus: Some(3) });
        assert_eq!(active_windows(&m, 0), vec![3]);
        assert_eq!(sidebar(&m, 0)[0], vec![1], "la fenêtre détachée arrive en tête de barre");
        // La dernière fenêtre d'une scène ne se détache pas.
        assert!(m.detach(3).is_empty());
        assert!(!m.can_detach(3));
    }

    #[test]
    fn eclater_une_scene_de_la_barre() {
        let (mut m, _) = model();
        let edge = m.stage_of(1).unwrap();
        assert!(m.split(edge));
        assert_eq!(sidebar(&m, 0), vec![vec![1], vec![3], vec![4]]);
        // La scène active ne s'éclate pas depuis la barre.
        assert!(!m.split(m.active(0).unwrap()));
    }

    #[test]
    fn adoption_sans_scene_active() {
        let mut m = Model::default();
        m.adopt(7, "paint", 0);
        assert_eq!(active_windows(&m, 0), vec![7]);
        m.adopt(8, "word", 0);
        assert_eq!(active_windows(&m, 0), vec![7, 8]);
    }

    #[test]
    fn une_scene_active_par_ecran() {
        let (m, t) = two_screens();
        // Écran 0 : la fenêtre au premier plan ; écran 1 : la plus haute dans l'ordre Z.
        assert_eq!(active_windows(&m, 0), vec![2]);
        assert_eq!(active_windows(&m, 1), vec![3]);
        let mut minimized = t.minimize.clone();
        minimized.sort();
        assert_eq!(minimized, vec![1, 4]);
        assert_eq!(sidebar(&m, 0), vec![vec![1]]);
        assert_eq!(sidebar(&m, 1), vec![vec![4]]);
    }

    #[test]
    fn changer_de_scene_ne_touche_pas_aux_autres_ecrans() {
        let (mut m, _) = two_screens();
        let explorer = m.stage_of(4).unwrap();
        let t = m.activate(explorer, None);
        assert_eq!(t, Transition { minimize: vec![3], restore: vec![], focus: Some(4) });
        assert_eq!(active_windows(&m, 0), vec![2], "l'écran 0 garde sa scène");
        assert!(m.in_active_stage(2) && m.in_active_stage(4));
        assert!(!m.in_active_stage(3));
    }

    #[test]
    fn une_nouvelle_fenetre_rejoint_les_scenes_de_son_ecran() {
        let (mut m, _) = two_screens();
        // Même appli que la scène active de l'écran 1 : elle la rejoint.
        assert!(m.add_focused(5, "edge", 1).is_empty());
        assert_eq!(active_windows(&m, 1), vec![5, 3]);
        // Nouvelle appli sur l'écran 1 : seule la scène de cet écran rejoint la barre.
        let t = m.add_focused(6, "notepad", 1);
        assert_eq!(t, Transition { minimize: vec![5, 3], restore: vec![], focus: Some(6) });
        assert_eq!(active_windows(&m, 0), vec![2]);
        assert_eq!(m.screen_of(6), Some(1));
    }

    #[test]
    fn une_fenetre_deplacee_rejoint_la_scene_active_de_son_nouvel_ecran() {
        let (mut m, _) = two_screens();
        assert_eq!(m.move_window(2, 1), Some(0));
        assert_eq!(active_windows(&m, 1), vec![2, 3]);
        assert_eq!(m.screen_of(2), Some(1));
        // L'écran 0 a perdu sa scène active : la suivante peut la remplacer.
        let edge = m.stage_of(1).unwrap();
        assert_eq!(m.next_stage(0), Some(edge));
        // Déjà sur cet écran : rien ne change.
        assert_eq!(m.move_window(2, 1), None);
    }

    #[test]
    fn un_ecran_qui_disparait_passe_ses_scenes_a_un_autre() {
        let (mut m, _) = two_screens();
        let t = m.move_screen(1, 0);
        assert_eq!(t, Transition { minimize: vec![3], ..Default::default() });
        assert_eq!(active_windows(&m, 0), vec![2]);
        let mut bar = sidebar(&m, 0);
        bar.sort();
        assert_eq!(bar, vec![vec![1], vec![3], vec![4]]);
        assert_eq!(m.active(1), None);
        // Vers un écran sans scène active : sa scène active le reste.
        let (mut m, _) = two_screens();
        m.remove(2);
        m.remove(1);
        assert!(m.move_screen(1, 0).is_empty());
        assert_eq!(active_windows(&m, 0), vec![3]);
    }
}
