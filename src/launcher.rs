//! Lanceur façon Spotlight : la liste des applis et la recherche (testable, sans Win32).

/// Une appli qu'on peut lancer.
pub struct Entry {
    pub name: String,
    /// Chemin à ouvrir avec le shell (« shell:AppsFolder\… »), qui marche aussi pour le Store.
    pub path: String,
    key: String,
}

impl Entry {
    pub fn new(name: &str, path: &str) -> Self {
        Self { name: name.to_owned(), path: path.to_owned(), key: normalize(name) }
    }
}

/// Minuscules, sans accents ni ponctuation : « Paramètres » et « parametres » se valent.
pub fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.to_lowercase().chars() {
        match c {
            'à' | 'â' | 'ä' | 'á' | 'ã' => out.push('a'),
            'é' | 'è' | 'ê' | 'ë' => out.push('e'),
            'î' | 'ï' | 'í' => out.push('i'),
            'ô' | 'ö' | 'ó' => out.push('o'),
            'ù' | 'û' | 'ü' | 'ú' => out.push('u'),
            'ç' => out.push('c'),
            'œ' => out.push_str("oe"),
            c if c.is_alphanumeric() => out.push(c),
            _ => out.push(' '),
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Score d'une appli pour une recherche (plus petit = meilleur), ou `None` si elle ne correspond pas.
fn score(key: &str, query: &str) -> Option<u32> {
    if key.starts_with(query) {
        return Some(0);
    }
    if key.split(' ').any(|word| word.starts_with(query)) {
        return Some(1);
    }
    if key.contains(query) {
        return Some(2);
    }
    // Lettres dans l'ordre, pas forcément collées (« vsc » trouve « Visual Studio Code »).
    let mut chars = key.chars();
    query.chars().filter(|c| *c != ' ').all(|q| chars.any(|c| c == q)).then_some(3)
}

/// Indices des meilleures applis pour `query`, au plus `limit`.
pub fn search(entries: &[Entry], query: &str, limit: usize) -> Vec<usize> {
    let query = normalize(query);
    if query.is_empty() {
        return Vec::new();
    }
    let mut found: Vec<(u32, usize, usize)> = entries
        .iter()
        .enumerate()
        .filter_map(|(i, e)| score(&e.key, &query).map(|s| (s, e.name.len(), i)))
        .collect();
    // Meilleur score d'abord, puis le nom le plus court (le plus proche de ce qu'on a tapé).
    found.sort();
    found.into_iter().take(limit).map(|(_, _, i)| i).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries() -> Vec<Entry> {
        ["Paramètres", "Visual Studio Code", "Discord", "Steam", "Microsoft Store", "Disque - Nettoyage"]
            .iter()
            .map(|n| Entry::new(n, n))
            .collect()
    }

    fn names(e: &[Entry], q: &str) -> Vec<String> {
        search(e, q, 10).into_iter().map(|i| e[i].name.clone()).collect()
    }

    #[test]
    fn sans_accents_ni_majuscules() {
        assert_eq!(normalize("Paramètres  du  Système"), "parametres du systeme");
        assert_eq!(names(&entries(), "parame"), vec!["Paramètres"]);
    }

    #[test]
    fn le_debut_du_nom_passe_avant_un_mot_puis_le_reste() {
        let e = entries();
        assert_eq!(names(&e, "dis"), vec!["Discord", "Disque - Nettoyage"]);
        assert_eq!(names(&e, "store"), vec!["Microsoft Store"]);
        assert_eq!(names(&e, "vsc"), vec!["Visual Studio Code"]);
    }

    #[test]
    fn recherche_vide_ou_sans_resultat() {
        assert!(names(&entries(), "  ").is_empty());
        assert!(names(&entries(), "zzz").is_empty());
    }
}
