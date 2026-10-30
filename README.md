# Stage Manager pour Windows

Une reproduction du Stage Manager de macOS pour Windows 11, écrite en Rust.

Une barre transparente sur le bord gauche de l'écran affiche les autres « scènes » sous forme
de cartes inclinées en perspective, comme sur macOS, posées sur le fond d'écran. La scène active reste seule à l'écran ;
les autres sont réduites.

## Utilisation

```bash
cargo run --release
```

- **Clic** sur une miniature : affiche cette scène et range la scène active dans la barre.
- **Glisser une miniature hors de la barre** (ou Maj + clic, ou clic droit → « Ajouter à la
  scène active ») : regroupe les fenêtres de cette scène avec la scène active. La relâcher
  sur la barre annule.
- **Séparer une fenêtre de sa scène** : la faire glisser par sa barre de titre jusque sur la
  barre latérale (qui s'éclaire), ou **Ctrl + Alt + D** sur la fenêtre active. Elle rejoint la
  barre comme scène à part, et reviendra à sa place d'origine quand on la rouvrira.
- **Clic droit sur une carte → « Séparer les fenêtres »** : éclate une scène regroupée en une
  scène par fenêtre.
- **Lecteur de musique** collé à gauche du Dock : pochette, titre, artiste et bouton
  lecture/pause du morceau en cours (Apple Music ou YouTube dans le navigateur, Spotify,
  Deezer…), sur une plaque de « verre liquide ». Il n'apparaît que lorsqu'un média est ouvert.
  Boutons précédent / lecture-pause / suivant, et barre de progression quand le lecteur
  donne la durée du morceau.
- **Horloge** collée à droite du Dock : l'heure et la date ; un clic ouvre le calendrier et
  les notifications de Windows. À sa gauche, le **volume** : un clic ouvre un curseur au-dessus
  de l'horloge (glisser pour régler, clic sur son haut-parleur pour couper le son), la molette
  le règle aussi, le pourcentage s'affiche au survol.
- **Lanceur façon Spotlight** : **Alt + Espace** ouvre une barre de recherche en verre ; tapez
  quelques lettres (accents et majuscules ignorés), ↑ ↓ pour choisir, Entrée pour lancer,
  Échap pour fermer. Il trouve toutes les applis installées, y compris celles du Store.
- Le verre du Dock et de ses panneaux est calculé à partir du fond d'écran (seule chose
  derrière eux), lu et flouté une seule fois.
- **Dock** en bas de l'écran, comme sur macOS : applis épinglées (reprises au départ de la
  barre des tâches), applis ouvertes à la suite, puis la corbeille. Les icônes grossissent en
  vague sous la souris, le nom de l'appli s'affiche au-dessus, un point marque les applis
  ouvertes. Clic : lance l'appli (l'icône rebondit) ou affiche sa scène. Clic droit : garder
  dans le Dock, quitter ; vider la corbeille. Les applis épinglées sont enregistrées dans
  `%APPDATA%\StageManager\dock.txt`. La barre des tâches de Windows passe en masquage
  automatique (remise comme avant en quittant) : poussez la souris tout en bas de l'écran
  pour retrouver l'horloge et les icônes de notification.
- **Ctrl + Alt + S** : active ou désactive le Stage Manager (toutes les fenêtres sont restaurées).
  Désactiver puis réactiver recrée les scènes, une par application.
- **Icône dans la zone de notification** (clic droit) : activer/désactiver, lancer au
  démarrage de Windows, quitter. Windows 11 la range d'abord dans les icônes cachées (^) :
  glissez-la dans la barre des tâches pour la garder visible.

### Démarrage avec Windows

L'option du menu enregistre l'exécutable dans la clé `Run` de l'utilisateur
(`HKCU\Software\Microsoft\Windows\CurrentVersion\Run`, valeur `StageManager`). Si
l'exécutable change de place, la version release met la clé à jour à son lancement.
Attention : `cargo clean` supprime `target\release\stage-manager.exe` ; il faudra recompiler
pour que le lancement automatique fonctionne de nouveau.

### Police

L'interface utilise [Inter](https://github.com/rsms/inter) (Regular et SemiBold), la police
libre la plus proche de SF Pro, intégrée à l'exécutable et chargée pour ce seul processus
(rien n'est installé dans Windows). Licence SIL Open Font : `assets/fonts/LICENSE-Inter.txt`.

### Icône

`assets/icon.ico` est généré par `assets/make-icon.ps1` (aperçu : `assets/icon-preview.png`)
et intégré à l'exécutable par `build.rs`.

Au démarrage, les fenêtres ouvertes sont regroupées par application : une scène par appli.
Ensuite :

- une nouvelle fenêtre d'une appli déjà présente dans la scène active la rejoint ;
- une nouvelle appli ouvre sa propre scène ;
- passer à une fenêtre par Alt+Tab ou la barre des tâches change de scène ;
- réduire une fenêtre la retire de sa scène.

Les fenêtres « toujours au premier plan » (vidéo en incrustation, fenêtre épinglée…) ne
font partie d'aucune scène : elles flottent au-dessus de toutes, comme sur macOS.

En quittant (ou en cas de plantage), toutes les fenêtres réduites par l'application sont restaurées.

## Animations

Quand on change de scène, les fenêtres sortantes rejoignent la barre et les fenêtres entrantes
en partent pour reprendre leur place ; les cartes de la barre glissent pour se réorganiser.
Les mouvements suivent un ressort amorti (léger dépassement puis retour, comme sur macOS) et
les miniatures qui apparaissent arrivent en fondu.
La durée (650 ms par défaut) se règle avec une variable d'environnement, `0` pour les couper :

```bash
STAGE_MANAGER_ANIM_MS=600 cargo run --release
```

Ce sont des miniatures DWM qui se déplacent sur une fenêtre transparente posée par-dessus
l'écran, pendant que les vraies fenêtres sont réduites ou restaurées en coulisse, sans
l'animation de Windows (`DWMWA_TRANSITIONS_FORCEDISABLED`). Le rythme est calé sur le
rafraîchissement de l'écran (`DwmFlush`).

## Architecture

| Fichier | Rôle |
|---|---|
| `src/stages.rs` | Modèle des scènes, sans Win32, couvert par des tests (`cargo test`) |
| `src/win.rs` | Utilitaires Win32 : filtrage des fenêtres, processus, icônes, premier plan |
| `src/app.rs` | Barre latérale (AppBar), miniatures DWM, icône de notification, événements |
| `src/anim.rs` | Animations : fenêtre transparente, trajectoires, courbes d'accélération |
| `src/canvas.rs` | Dessin avec transparence par pixel pour la barre, projection en perspective |
| `src/dock.rs` | Modèle du Dock : éléments, épinglage, grossissement, rebond (testé) |
| `src/app/dockbar.rs` | Fenêtre du Dock : place réservée, dessin, clics, menu |
| `src/shell.rs` | Raccourcis épinglés, grandes icônes, noms d'applis, lancement, corbeille |
| `src/taskbar.rs` | Masquage automatique de la barre des tâches, et sa remise en état |
| `src/wallpaper.rs` | Fond d'écran derrière la barre, flouté, pour l'effet verre (fil à part) |
| `src/launcher.rs` | Lanceur : recherche des applis (testée) |
| `src/app/launcherui.rs` | Fenêtre du lanceur : saisie, résultats, verre |
| `src/volume.rs` | Volume de la sortie audio par défaut |
| `src/media.rs` | Morceau en cours via les commandes multimédias de Windows (fil à part) |
| `src/snapshot.rs` | Instantané de chaque fenêtre juste avant qu'elle soit réduite (`PrintWindow`) |
| `src/autostart.rs` | Lancement automatique à l'ouverture de session |
| `build.rs`, `assets/` | Icône de l'application et son intégration dans l'exécutable |
| `src/main.rs` | Point d'entrée, instance unique |

Briques Windows utilisées : `SetWinEventHook` (focus, réduction, fermeture),
`DwmRegisterThumbnail` (miniatures en direct), `SHAppBarMessage` (réserver le bord de l'écran
comme la barre des tâches), `ShowWindowAsync` (une appli figée ne bloque pas la nôtre).

## Limites connues

- Les fenêtres lancées en administrateur ne sont pas gérées (Windows l'interdit), sauf si
  Stage Manager est lui-même lancé en administrateur.
- La barre n'apparaît que sur l'écran principal.
- Les bureaux virtuels ne sont pas encore pris en compte.
- Les cartes de la barre sont des instantanés pris quand la fenêtre a quitté l'écran (les
  miniatures DWM, elles, ne peuvent pas être inclinées) : elles ne suivent pas les changements
  de contenu d'une fenêtre réduite. Pendant le trajet, les miniatures restent plates et se
  fondent dans la carte inclinée à l'arrivée.
- Les applis Chromium/Electron (Discord, Chrome…) cessent de se dessiner quand elles restent
  longtemps réduites : leur miniature volante peut être noire au début du trajet.
