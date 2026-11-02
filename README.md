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
- **Encoche** en haut de l'écran, au milieu, comme celle des MacBook Pro : noire, collée au
  bord, raccordée à lui par deux petits congés. Fermée, elle affiche l'heure. Pendant la
  lecture, elle s'élargit un peu : l'heure glisse à gauche et un petit symbole de lecture
  apparaît à droite (de la couleur de la pochette). Au survol (ou au clic), elle
  s'agrandit d'un mouvement de ressort et montre le lecteur de musique : grande pochette,
  titre, artiste, temps écoulé et restant, précédent / lecture-pause / suivant. Il suit le
  morceau en cours (Apple Music ou YouTube dans le navigateur, Spotify, Deezer…). Elle se
  referme quand la souris s'en va, et s'efface devant un jeu ou une vidéo en plein écran.
- **Page accrochée à l'encoche** (un graphique TradingView, par exemple) : **Ctrl + Alt + P**
  sur une fenêtre de navigateur accroche la page affichée (adresse lue dans la barre
  d'adresse, ou à défaut copiée dans le presse-papiers) ; clic droit sur l'encoche →
  « Accrocher la page active » fait de même. La page s'ouvre dans une fenêtre Chrome à part,
  avec son propre profil (`%LOCALAPPDATA%\StageManager\Chrome` : on s'y connecte une fois),
  qui ne rejoint aucune scène. Elle est garée hors de l'écran plutôt que réduite, et Chrome y
  est lancé sans sa détection des fenêtres cachées : la page continue de se dessiner.
  L'encoche fermée affiche son titre ; pour TradingView, le symbole, le cours et la variation
  du jour (en vert ou en rouge). Ouverte, elle montre la page en direct, sous le lecteur.
  Un clic sur l'encoche amène la page à l'écran, au premier plan ; elle retourne au garage au
  clic suivant ou dès qu'une autre fenêtre passe devant (la réduire la gare aussi). Sa taille
  règle le zoom de l'aperçu. Fermée, elle se rouvre d'un clic sur l'encoche, et au démarrage
  du Stage Manager. Ctrl + Alt + P sur la page elle-même (ou clic droit → « Décrocher la
  page ») la ferme et la décroche. L'adresse est enregistrée dans
  `%APPDATA%\StageManager\notch.txt`.
- **Coins de l'écran arrondis**, comme sur les Mac : un quart de cercle noir dans chaque coin
  (20 pixels à 100 %), par-dessus tout, que la souris traverse. Ils s'effacent devant un jeu ou
  une vidéo en plein écran, pour ne pas lui ajouter de latence.
- **Volume** dans un petit panneau collé à droite du Dock : un clic ouvre un curseur
  au-dessus (glisser pour régler, clic sur son haut-parleur pour couper le son), la molette
  le règle aussi, le pourcentage s'affiche au survol.
- **Lanceur façon Spotlight** : **Alt + Espace** ouvre une barre de recherche ; tapez
  quelques lettres (accents et majuscules ignorés), ↑ ↓ pour choisir, Entrée pour lancer,
  Échap pour fermer. Il trouve toutes les applis installées, y compris celles du Store.
- Le Dock, ses panneaux et le lanceur suivent le style de macOS d'avant Tahoe (mode sombre) :
  panneaux gris foncé presque pleins, à peine teintés par le fond flouté, avec un fin liseré.
- **Dock** en bas de l'écran, comme sur macOS : applis épinglées (reprises au départ de la
  barre des tâches), applis ouvertes à la suite, puis la corbeille. Les icônes grossissent en
  vague sous la souris, le nom de l'appli s'affiche au-dessus, un point marque les applis
  ouvertes. Clic : lance l'appli (l'icône rebondit) ou affiche sa scène. Clic droit : garder
  dans le Dock, quitter ; vider la corbeille. Les applis épinglées sont enregistrées dans
  `%APPDATA%\StageManager\dock.txt`. La barre des tâches de Windows passe en masquage
  automatique (remise comme avant en quittant) : poussez la souris tout en bas de l'écran
  pour retrouver l'horloge et les icônes de notification.
- **Réglages** : chaque fonctionnalité s'active ou se désactive avec son interrupteur, à la
  manière de macOS — le Stage Manager (les scènes et leur barre), le Dock, l'encoche, la page
  accrochée (grisée sans l'encoche), le lanceur, les coins arrondis — ainsi que le lancement au
  démarrage de Windows. Le changement est immédiat : sans les scènes, toutes les fenêtres
  sont restaurées ; sans le Dock, la barre des tâches de Windows revient ; sans le lanceur,
  Alt + Espace retrouve son rôle habituel. Les réglages s'ouvrent d'un clic sur l'icône de
  notification, par le clic droit sur l'encoche, ou en relançant l'application. Ils sont
  enregistrés dans `%APPDATA%\StageManager\settings.txt` (tout est activé par défaut).
- **Ctrl + Alt + S** : active ou désactive le Stage Manager, c'est-à-dire les scènes (toutes
  les fenêtres sont restaurées) ; le reste ne bouge pas. Réactiver recrée les scènes, une par
  application.
- **Icône dans la zone de notification** : clic pour les réglages ; clic droit pour les
  réglages, le lancement au démarrage de Windows, quitter. Windows 11 la range d'abord dans
  les icônes cachées (^) : glissez-la dans la barre des tâches pour la garder visible.

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
| `src/wallpaper.rs` | Fond d'écran derrière le Dock, flouté, pour la teinte des panneaux (fil à part) |
| `src/launcher.rs` | Lanceur : recherche des applis (testée) |
| `src/app/launcherui.rs` | Fenêtre du lanceur : saisie, résultats |
| `src/app/notch.rs` | Encoche en haut de l'écran : formes, animation, lecteur complet |
| `src/app/pin.rs` | Page accrochée à l'encoche : fenêtre Chrome dédiée, garage hors écran, cours |
| `src/app/corners.rs` | Coins arrondis de l'écran |
| `src/settings.rs` | Fonctionnalités activées ou non, et leur enregistrement (testé) |
| `src/app/settingsui.rs` | Fenêtre des réglages : une ligne et un interrupteur par fonctionnalité |
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
- La barre, l'encoche et les coins arrondis n'apparaissent que sur l'écran principal.
- L'encoche recouvre le milieu du haut de l'écran (environ 190 × 32 pixels à 100 %) : un clic
  à cet endroit l'ouvre au lieu d'atteindre la fenêtre en dessous (onglet, barre de titre).
- Les bureaux virtuels ne sont pas encore pris en compte.
- Les cartes de la barre sont des instantanés pris quand la fenêtre a quitté l'écran (les
  miniatures DWM, elles, ne peuvent pas être inclinées) : elles ne suivent pas les changements
  de contenu d'une fenêtre réduite. Pendant le trajet, les miniatures restent plates et se
  fondent dans la carte inclinée à l'arrivée.
- Les applis Chromium/Electron (Discord, Chrome…) cessent de se dessiner quand elles restent
  longtemps réduites : leur miniature volante peut être noire au début du trajet.
