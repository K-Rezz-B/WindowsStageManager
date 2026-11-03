//! Chats Claude Code récents, et ce qu'ils font : en cours, en attente d'une réponse (question,
//! autorisation…) ou finis.
//!
//! Deux sources, toutes deux internes (leur format peut changer d'une version à l'autre) :
//! - l'appli Claude garde un fichier par chat dans `%APPDATA%\Claude\claude-code-sessions` :
//!   titre, dossier, dernière activité, et le résumé de fin de tour (qui dit si le chat attend
//!   quelque chose). Elle y range aussi les chats qui tournent sur un serveur, en SSH ;
//! - chaque Claude Code qui tourne sur ce PC (dans l'appli comme dans un terminal) tient à jour
//!   `%USERPROFILE%\.claude\sessions\<pid>.json`, avec son état à l'instant : plus immédiat,
//!   il l'emporte quand il existe. Ces fichiers restent parfois après la fermeture du chat : on
//!   vérifie que leur processus tourne encore.
//!
//! Un fil relit ces fichiers dès que Windows signale un changement dans l'un des deux dossiers,
//! et prévient l'encoche par un message quand la liste change.

use std::{
    cmp::Reverse,
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use windows::{
    core::HSTRING,
    Win32::{
        Foundation::{CloseHandle, FILETIME, HANDLE, HWND, LPARAM, WAIT_OBJECT_0, WPARAM},
        Storage::FileSystem::{
            FindCloseChangeNotification, FindFirstChangeNotificationW, FindNextChangeNotification, FILE_NOTIFY_CHANGE_FILE_NAME,
            FILE_NOTIFY_CHANGE_LAST_WRITE,
        },
        System::{
            Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS},
            Threading::{GetProcessTimes, OpenProcess, WaitForMultipleObjects, PROCESS_QUERY_LIMITED_INFORMATION},
        },
        UI::WindowsAndMessaging::PostMessageW,
    },
};

/// Sans nouvelle des dossiers, les fichiers sont tout de même relus à ce rythme, en
/// millisecondes : un chat fermé d'un coup (processus tué) ne touche pas à son fichier.
const TICK: u32 = 1000;
/// Après un changement, le temps que Claude finisse d'écrire.
const SETTLE: Duration = Duration::from_millis(25);
/// Les chats de l'appli sans activité depuis ce temps ne sont plus montrés (en millisecondes).
const RECENT: u64 = 24 * 3600 * 1000;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Status {
    Busy,
    /// Attend une réponse : une question, une autorisation, un dialogue ouvert.
    Waiting,
    Idle,
}

/// Où tourne le chat.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Place {
    /// Dans l'appli Claude, sur ce PC.
    Desktop,
    /// Dans l'appli Claude, sur un serveur.
    Ssh,
    Terminal,
}

/// Un chat.
#[derive(Clone, PartialEq, Debug)]
pub struct Chat {
    /// Identifiant de la session Claude Code : le même dans les deux sources.
    pub session: String,
    pub name: String,
    pub status: Status,
    /// Dossier du projet (son dernier élément).
    pub project: String,
    pub place: Place,
    /// Processus de Claude Code qui porte le chat sur ce PC (0 : aucun).
    pub pid: u32,
    /// Date du dernier changement, en millisecondes depuis 1970.
    pub since: u64,
    /// Ce que le chat attend de nous, quand il attend.
    pub detail: Option<String>,
}

pub struct Claude {
    chats: Arc<Mutex<Vec<Chat>>>,
}

/// Dernière lecture des fichiers, pour ne relire que ceux qui ont changé.
#[derive(Default)]
struct Cache {
    /// Fichiers de l'appli : date de modification et chat (aucun s'il est archivé).
    desktop: HashMap<PathBuf, (SystemTime, Option<Chat>)>,
    /// Fichiers des Claude Code de ce PC.
    local: HashMap<PathBuf, (u64, Chat)>,
}

impl Claude {
    /// Démarre le suivi ; `message` est posté à `notify` quand la liste change.
    pub fn start(notify: HWND, message: u32) -> Self {
        let chats = Arc::new(Mutex::new(Vec::new()));
        let shared = Arc::clone(&chats);
        let notify = notify.0 as isize;
        thread::spawn(move || {
            let mut cache = Cache::default();
            let mut before: Vec<Chat> = Vec::new();
            // Une veille par dossier ; ceux qui n'existent pas encore sont retentés.
            let mut watches: [Option<HANDLE>; 2] = [None, None];
            loop {
                let now = read_chats(&mut cache, now_ms());
                if now != before {
                    if let Ok(mut c) = shared.lock() {
                        c.clone_from(&now);
                    }
                    before = now;
                    unsafe {
                        let _ = PostMessageW(HWND(notify as _), message, WPARAM(0), LPARAM(0));
                    }
                }
                for (watch, dir) in watches.iter_mut().zip([desktop_dir(), sessions_dir()]) {
                    if watch.is_none() {
                        *watch = dir.and_then(|dir| unsafe {
                            let filter = FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_LAST_WRITE;
                            FindFirstChangeNotificationW(&HSTRING::from(dir.as_os_str()), true, filter).ok()
                        });
                    }
                }
                let handles: Vec<HANDLE> = watches.iter().flatten().copied().collect();
                if handles.is_empty() {
                    thread::sleep(Duration::from_millis(TICK as u64));
                    continue;
                }
                let woke = unsafe { WaitForMultipleObjects(&handles, false, TICK) }.0.wrapping_sub(WAIT_OBJECT_0.0) as usize;
                if let Some(&handle) = handles.get(woke) {
                    thread::sleep(SETTLE);
                    if unsafe { FindNextChangeNotification(handle) }.is_err() {
                        unsafe {
                            let _ = FindCloseChangeNotification(handle);
                        }
                        for watch in &mut watches {
                            if *watch == Some(handle) {
                                *watch = None;
                            }
                        }
                    }
                }
            }
        });
        Self { chats }
    }

    /// Les chats : ceux qui attendent une réponse, puis les plus récemment actifs.
    pub fn chats(&self) -> Vec<Chat> {
        self.chats.lock().map(|c| c.clone()).unwrap_or_default()
    }
}

fn sessions_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE").map(|home| PathBuf::from(home).join(".claude").join("sessions"))
}

fn desktop_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|data| PathBuf::from(data).join("Claude").join("claude-code-sessions"))
}

fn read_chats(cache: &mut Cache, now: u64) -> Vec<Chat> {
    read_desktop(&mut cache.desktop);
    read_local(&mut cache.local);
    let desktop = cache.desktop.values().filter_map(|(_, chat)| chat.as_ref());
    let local = cache.local.values().map(|(_, chat)| chat);
    merge(desktop, local, now)
}

/// Réunit les deux sources : les chats récents de l'appli, avec l'état à l'instant de ceux qui
/// tournent sur ce PC, et les chats des terminaux.
fn merge<'a>(desktop: impl Iterator<Item = &'a Chat>, local: impl Iterator<Item = &'a Chat>, now: u64) -> Vec<Chat> {
    let mut out: Vec<Chat> = Vec::new();
    let desktop: HashMap<&str, &Chat> = desktop.map(|c| (c.session.as_str(), c)).collect();
    for live in local {
        let Some(&known) = desktop.get(live.session.as_str()) else {
            out.push(live.clone());
            continue;
        };
        // Un chat qui finit son tour sur une question repasse « idle » ici : le résumé de
        // l'appli, lui, sait qu'il attend une réponse.
        let status = match (live.status, known.status) {
            (Status::Idle, Status::Waiting) => Status::Waiting,
            (status, _) => status,
        };
        let detail = if status == Status::Waiting { known.detail.clone() } else { None };
        out.push(Chat { status, detail, pid: live.pid, since: live.since.max(known.since), ..known.clone() });
    }
    for chat in desktop.values() {
        let recent = now.saturating_sub(chat.since) < RECENT;
        if recent && !out.iter().any(|c| c.session == chat.session) {
            out.push((*chat).clone());
        }
    }
    order(&mut out);
    out
}

/// Les chats qui attendent une réponse d'abord, puis les plus récemment actifs.
fn order(chats: &mut [Chat]) {
    chats.sort_by(|a, b| {
        let key = |c: &Chat| (c.status != Status::Waiting, Reverse(c.since));
        key(a).cmp(&key(b)).then_with(|| a.session.cmp(&b.session))
    });
}

/// Ce qu'on tire d'un fichier de chat.
#[derive(PartialEq, Debug)]
enum Read {
    /// Le chat, et sa date de début (fichiers de ce PC) ou de dernière activité (appli).
    Open((u64, Chat)),
    /// Chat fermé ou archivé, ou qui ne nous concerne pas.
    Closed,
    /// Fichier incomplet : en cours d'écriture.
    Partial,
}

// --- Fichiers de l'appli Claude -------------------------------------------------------------

/// Relit les fichiers de l'appli qui ont changé : `<compte>\<organisation>\local_<id>.json`.
fn read_desktop(cache: &mut HashMap<PathBuf, (SystemTime, Option<Chat>)>) {
    let mut seen = Vec::new();
    let dirs = |dir: &Path| std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect::<Vec<_>>();
    for account in desktop_dir().as_deref().map(dirs).unwrap_or_default() {
        for org in dirs(&account) {
            for entry in std::fs::read_dir(&org).into_iter().flatten().flatten() {
                let path = entry.path();
                if !entry.file_name().to_string_lossy().starts_with("local_") {
                    continue;
                }
                let Ok(modified) = entry.metadata().and_then(|m| m.modified()) else { continue };
                seen.push(path.clone());
                if cache.get(&path).is_some_and(|(at, _)| *at == modified) {
                    continue;
                }
                let text = std::fs::read_to_string(&path).unwrap_or_default();
                match parse_desktop(&text) {
                    Read::Open((_, chat)) => cache.insert(path, (modified, Some(chat))),
                    Read::Closed => cache.insert(path, (modified, None)),
                    // Relu au prochain passage.
                    Read::Partial => None,
                };
            }
        }
    }
    cache.retain(|path, _| seen.contains(path));
}

fn parse_desktop(text: &str) -> Read {
    let Some(id) = string_field(text, "sessionId").filter(|_| text.trim_end().ends_with('}')) else {
        return Read::Partial;
    };
    if value_start(text, "isArchived").is_some_and(|i| text[i..].starts_with("true")) {
        return Read::Closed;
    }
    let session = string_field(text, "cliSessionId").unwrap_or(id);
    let activity = number_field(text, "lastActivityAt").unwrap_or(0);
    // Un message envoyé après la fin du dernier tour : le chat y travaille.
    let busy = number_field(text, "latestUserFrameAt").unwrap_or(0) > number_field(text, "lastTurnEdgeAt").unwrap_or(0);
    // Le résumé de fin de tour, s'il porte bien sur la dernière réponse, dit si le chat attend.
    let fresh = string_field(text, "postTurnSummaryFor").is_some_and(|s| Some(s) == string_field(text, "lastAssistantUuid"));
    let blocked = fresh && string_field(text, "status_category").is_some_and(|c| c == "blocked");
    let status = match (busy, blocked) {
        (true, _) => Status::Busy,
        (false, true) => Status::Waiting,
        (false, false) => Status::Idle,
    };
    let detail = blocked.then(|| string_field(text, "needs_action")).flatten().filter(|d| !d.trim().is_empty());
    let project = project(&string_field(text, "cwd").unwrap_or_default());
    let name = string_field(text, "title").filter(|n| !n.trim().is_empty()).unwrap_or_else(|| project.clone());
    let ssh = value_start(text, "sshConfig").is_some_and(|i| text[i..].starts_with('{'));
    let place = if ssh { Place::Ssh } else { Place::Desktop };
    Read::Open((activity, Chat { session, name, status, project, place, pid: 0, since: activity, detail }))
}

// --- Fichiers des Claude Code de ce PC -----------------------------------------------------

/// Relit les fichiers des Claude Code de ce PC. Un fichier pris en pleine écriture (incomplet)
/// garde son chat d'avant, au lieu de le faire disparaître.
fn read_local(known: &mut HashMap<PathBuf, (u64, Chat)>) {
    let entries = sessions_dir().and_then(|dir| std::fs::read_dir(dir).ok());
    let mut next = HashMap::new();
    for entry in entries.into_iter().flatten().flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let found = match parse_local(&text) {
            Read::Open(found) => Some(found),
            Read::Closed => None,
            Read::Partial => keep(&path, known),
        };
        if let Some(found) = found {
            next.insert(path, found);
        }
    }
    *known = next;
}

/// Le chat lu juste avant dans ce fichier, si son processus tourne toujours.
fn keep(path: &Path, known: &HashMap<PathBuf, (u64, Chat)>) -> Option<(u64, Chat)> {
    known.get(path).filter(|(_, chat)| running(chat.pid, None)).cloned()
}

fn parse_local(text: &str) -> Read {
    let (Some(pid), Some(status)) = (number_field(text, "pid"), string_field(text, "status")) else {
        return Read::Partial;
    };
    // Les tâches de fond n'ont personne à qui poser de question.
    if string_field(text, "kind").is_some_and(|k| k != "interactive") {
        return Read::Closed;
    }
    let pid = pid as u32;
    let started = string_field(text, "procStart").and_then(|s| s.parse().ok());
    if !running(pid, started) {
        return Read::Closed;
    }
    let status = match status.as_str() {
        "busy" => Status::Busy,
        "waiting" => Status::Waiting,
        _ => Status::Idle,
    };
    let session = string_field(text, "sessionId").unwrap_or_default();
    let project = project(&string_field(text, "cwd").unwrap_or_default());
    // Sans nom, le dossier du projet.
    let name = string_field(text, "name").filter(|n| !n.trim().is_empty()).unwrap_or_else(|| project.clone());
    let desktop = string_field(text, "entrypoint").is_some_and(|e| e == "claude-desktop");
    let place = if desktop { Place::Desktop } else { Place::Terminal };
    let since = number_field(text, "statusUpdatedAt").or_else(|| number_field(text, "updatedAt")).unwrap_or(0);
    let chat = Chat { session, name, status, project, place, pid, since, detail: None };
    Read::Open((number_field(text, "startedAt").unwrap_or(0), chat))
}

/// Dernier élément d'un dossier, Windows ou Linux.
fn project(cwd: &str) -> String {
    cwd.rsplit(['\\', '/']).find(|p| !p.is_empty()).unwrap_or("Claude Code").to_owned()
}

/// Millisecondes depuis 1970, comme les dates des fichiers.
pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// Le processus parent de chaque processus.
pub fn parents() -> HashMap<u32, u32> {
    let mut map = HashMap::new();
    unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return map };
        let mut entry = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut ok = Process32FirstW(snapshot, &mut entry).is_ok();
        while ok {
            map.insert(entry.th32ProcessID, entry.th32ParentProcessID);
            ok = Process32NextW(snapshot, &mut entry).is_ok();
        }
        let _ = CloseHandle(snapshot);
    }
    map
}

/// Le processus `pid` tourne, et c'est bien celui du fichier (un numéro de processus finit par
/// resservir) : `started` est sa date de création, en FILETIME.
fn running(pid: u32, started: Option<u64>) -> bool {
    unsafe {
        let Ok(process) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return false };
        let (mut created, mut exited, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
        let times = GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user);
        let _ = CloseHandle(process);
        let created = (created.dwHighDateTime as u64) << 32 | created.dwLowDateTime as u64;
        match (times, started) {
            // À une seconde près : la date notée et celle de Windows peuvent différer un peu.
            (Ok(()), Some(started)) => created.abs_diff(started) < 10_000_000,
            _ => true,
        }
    }
}

// --- Lecture du JSON ------------------------------------------------------------------------
//
// Les fichiers sont petits et plats : de quoi lire un champ texte ou nombre au premier niveau
// suffit, sans dépendance de plus.

/// Position juste après `"key":` (espaces sautés).
fn value_start(text: &str, key: &str) -> Option<usize> {
    let pattern = format!("\"{key}\"");
    let mut from = 0;
    while let Some(i) = text[from..].find(&pattern) {
        let after = from + i + pattern.len();
        let rest = text[after..].trim_start();
        if let Some(value) = rest.strip_prefix(':') {
            return Some(text.len() - value.trim_start().len());
        }
        from = after;
    }
    None
}

fn string_field(text: &str, key: &str) -> Option<String> {
    let start = value_start(text, key)?;
    let mut chars = text[start..].strip_prefix('"')?.chars();
    let mut out = String::new();
    loop {
        match chars.next()? {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                'b' | 'f' => {}
                'u' => {
                    let hex: String = chars.by_ref().take(4).collect();
                    let unit = u16::from_str_radix(&hex, 16).ok()?;
                    // Paire de substitution : le second morceau suit, en \uXXXX lui aussi.
                    if (0xD800..0xDC00).contains(&unit) {
                        let rest: String = chars.by_ref().take(6).collect();
                        let low = rest.strip_prefix("\\u").and_then(|h| u16::from_str_radix(h, 16).ok())?;
                        out.extend(char::decode_utf16([unit, low]).map(|c| c.unwrap_or('\u{FFFD}')));
                    } else {
                        out.push(char::from_u32(unit as u32).unwrap_or('\u{FFFD}'));
                    }
                }
                c => out.push(c),
            },
            c => out.push(c),
        }
    }
}

fn number_field(text: &str, key: &str) -> Option<u64> {
    let start = value_start(text, key)?;
    let digits: String = text[start..].chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lit_les_champs_d_un_chat() {
        let text = r#"{"pid":35004,"sessionId":"1704c574","cwd":"C:\\Dev\\Projet","startedAt":1793698217405,"kind":"interactive","name":"R\u00e9organiser la barre d'ic\u00f4nes","status":"waiting","waitingFor":"input needed"}"#;
        assert_eq!(number_field(text, "pid"), Some(35004));
        assert_eq!(string_field(text, "name").as_deref(), Some("Réorganiser la barre d'icônes"));
        assert_eq!(string_field(text, "cwd").as_deref(), Some("C:\\Dev\\Projet"));
        assert_eq!(string_field(text, "status").as_deref(), Some("waiting"));
        assert_eq!(string_field(text, "absent"), None);
    }

    #[test]
    fn une_cle_dans_une_valeur_ne_trompe_pas() {
        let text = r#"{"name":"le \"status\" du chat","status":"busy"}"#;
        assert_eq!(string_field(text, "status").as_deref(), Some("busy"));
        assert_eq!(string_field(text, "name").as_deref(), Some("le \"status\" du chat"));
    }

    #[test]
    fn un_chat_dont_le_processus_est_ferme_est_ignore() {
        // Le numéro 0 est le processus inactif du système, qu'on ne peut pas ouvrir.
        let text = r#"{"pid":0,"kind":"interactive","status":"idle","name":"Ancien"}"#;
        assert_eq!(parse_local(text), Read::Closed);
    }

    #[test]
    fn un_fichier_en_cours_d_ecriture_est_reconnu() {
        assert_eq!(parse_local(r#"{"pid":35004,"sessionId":"1704"#), Read::Partial);
        assert_eq!(parse_local(""), Read::Partial);
    }

    fn chat(session: &str, status: Status, since: u64) -> Chat {
        Chat {
            session: session.to_owned(),
            name: String::new(),
            status,
            project: String::new(),
            place: Place::Desktop,
            pid: 0,
            since,
            detail: None,
        }
    }

    fn sessions(chats: &[Chat]) -> Vec<&str> {
        chats.iter().map(|c| c.session.as_str()).collect()
    }

    #[test]
    fn les_questions_d_abord_puis_les_plus_recents() {
        let mut chats = vec![
            chat("a", Status::Idle, 300),
            chat("b", Status::Waiting, 100),
            chat("c", Status::Busy, 500),
            chat("d", Status::Waiting, 200),
        ];
        order(&mut chats);
        assert_eq!(sessions(&chats), ["d", "b", "c", "a"]);
    }

    #[test]
    fn lit_un_chat_de_l_appli() {
        let text = r#"{"sessionId":"local_44","cliSessionId":"91d1","cwd":"/home/WwwBSD/boutique","lastActivityAt":5000,"latestUserFrameAt":1000,"lastTurnEdgeAt":4000,"isArchived":false,"title":"Zone des prix","sshConfig":{"sshHost":"serveur"},"postTurnSummary":{"status_category":"blocked","needs_action":"Envoyer une capture"},"postTurnSummaryFor":"u2","lastAssistantUuid":"u2","schema":{"title":{"type":"string"}}}"#;
        let Read::Open((_, chat)) = parse_desktop(text) else { panic!("chat non lu") };
        assert_eq!(chat.session, "91d1");
        assert_eq!(chat.name, "Zone des prix");
        assert_eq!(chat.project, "boutique");
        assert_eq!(chat.place, Place::Ssh);
        assert_eq!(chat.status, Status::Waiting);
        assert_eq!(chat.detail.as_deref(), Some("Envoyer une capture"));
        assert_eq!(chat.since, 5000);
    }

    #[test]
    fn etat_d_un_chat_de_l_appli() {
        // Un message après la fin du dernier tour : en cours.
        let busy = r#"{"sessionId":"local_1","latestUserFrameAt":9000,"lastTurnEdgeAt":4000,"sshConfig":null}"#;
        let Read::Open((_, chat)) = parse_desktop(busy) else { panic!() };
        assert_eq!((chat.status, chat.place), (Status::Busy, Place::Desktop));
        // Un résumé « bloqué » d'une réponse précédente ne compte plus.
        let stale = r#"{"sessionId":"local_2","postTurnSummary":{"status_category":"blocked"},"postTurnSummaryFor":"u1","lastAssistantUuid":"u2"}"#;
        let Read::Open((_, chat)) = parse_desktop(stale) else { panic!() };
        assert_eq!(chat.status, Status::Idle);
        assert_eq!(parse_desktop(r#"{"sessionId":"local_3","isArchived":true}"#), Read::Closed);
        assert_eq!(parse_desktop(r#"{"sessionId":"local_4","title":"coup"#), Read::Partial);
    }

    #[test]
    fn fusion_des_deux_sources() {
        let now = 100 * RECENT;
        let mut question = chat("q", Status::Waiting, now - 1000);
        question.detail = Some("Choisir".to_owned());
        let desktop = [
            question,
            chat("vieux", Status::Idle, now - 2 * RECENT),
            chat("vieux-ouvert", Status::Idle, now - 2 * RECENT),
            chat("recent", Status::Idle, now - 5000),
        ];
        let mut live_q = chat("q", Status::Idle, now - 900);
        live_q.pid = 7;
        let local = [live_q, chat("vieux-ouvert", Status::Busy, now - 2 * RECENT), chat("terminal", Status::Busy, now - 100)];
        let chats = merge(desktop.iter(), local.iter(), now);
        // Le vieux chat fermé disparaît, celui qui tourne encore reste.
        assert_eq!(sessions(&chats), ["q", "terminal", "recent", "vieux-ouvert"]);
        // Repassé « idle » sur une question : il attend toujours, avec le processus de ce PC.
        assert_eq!((chats[0].status, chats[0].pid, chats[0].detail.as_deref()), (Status::Waiting, 7, Some("Choisir")));
        assert_eq!(chats[3].status, Status::Busy);
    }
}
