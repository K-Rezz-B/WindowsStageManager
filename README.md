# Stage Manager pour Windows

Une reproduction du Stage Manager de macOS pour Windows 11, écrite en Rust.

Une petite pastille noire en haut de chaque écran, collée à gauche de l'encoche, montre les autres
« scènes » par leur seule icône : ni nom, ni aperçu. La scène active reste seule à l'écran ; les
autres sont réduites.

## Installation

Il faut Windows 11, avec :

- **Rust**, installé par [rustup](https://rustup.rs) (chaîne `stable-x86_64-pc-windows-msvc`,
  celle par défaut) ;
- les **Build Tools de Visual Studio**, charge de travail « Développement Desktop en C++ »,
  avec le SDK Windows 11 : l'éditeur de liens MSVC, et `rc.exe` pour intégrer l'icône ;
- **Google Chrome**, seulement pour la page accrochée à l'encoche.

En ligne de commande, avec winget :

```bash
winget install Rustlang.Rustup
winget install Microsoft.VisualStudio.2022.BuildTools --override "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
```

Puis, dans un nouveau terminal :

```bash
git clone https://github.com/K-Rezz-B/WindowsStageManager.git
cd WindowsStageManager
cargo build --release
```

L'exécutable est `target\release\stage-manager.exe`. Lancez-le une fois, puis cochez
« Lancer au démarrage de Windows » dans ses Réglages pour qu'il démarre avec la session.
Les réglages sont propres à chaque PC (`%APPDATA%\StageManager\settings.txt`) : sur une
nouvelle machine, tout est activé.

## Utilisation

```bash
cargo run --release
```

- **Clic** sur une icône : affiche cette scène et range la scène active dans la barre.
- **Glisser une icône vers le bas, hors de la barre** (ou Maj + clic, ou clic droit → « Ajouter à la
  scène active ») : regroupe les fenêtres de cette scène avec la scène active. La relâcher
  sur la barre annule.
- **Séparer une fenêtre de sa scène** : la faire glisser par sa barre de titre jusque sur la
  pastille d'icônes (qui s'éclaire), ou **Ctrl + Alt + D** sur la fenêtre active. Elle rejoint la
  barre comme scène à part, et reviendra à sa place d'origine quand on la rouvrira.
- **Clic droit sur une carte → « Séparer les fenêtres »** : éclate une scène regroupée en une
  scène par fenêtre.
- **Encoche** en haut de chaque écran, au milieu, comme celle des MacBook Pro : noire, collée au
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
  Un clic sur l'encoche amène la page à l'écran de cette encoche, au premier plan ; elle
  retourne au garage au clic suivant ou dès qu'une autre fenêtre passe devant (la réduire la
  gare aussi). Sa taille règle le zoom de l'aperçu. Fermée, elle se rouvre d'un clic sur
  l'encoche, et au démarrage du Stage Manager. Ctrl + Alt + P sur la page elle-même (ou clic
  droit → « Décrocher la page ») la ferme et la décroche. L'adresse est enregistrée dans
  `%APPDATA%\StageManager\notch.txt`.
- **Fenêtres épinglées** : faites glisser n'importe quelle fenêtre (par sa barre de titre) vers
  le haut de l'écran. L'encoche s'élargit en zone de dépôt (« Déposer ici pour épingler », une
  punaise qui sautille), puis grandit et passe au bleu quand la fenêtre est au-dessus. Lâchée là,
  la fenêtre reprend sa place d'avant et reste **au premier plan**, devant toutes les autres.
  La réduire (bouton de réduction, Win + Bas) la change en **vignette** en direct dans le coin
  en haut à droite de son écran, avec son titre (le cours, pour la page TradingView). Un clic sur
  la vignette rouvre la fenêtre ; sa croix la désépingle. Redéposer la fenêtre sur l'encoche la
  désépingle aussi. Une seule fenêtre épinglée à la fois ; la vignette s'efface devant un jeu
  ou une vidéo en plein écran. La page accrochée s'épingle de la même façon (un clic sur
  l'encoche la fait alors passer de l'écran à sa vignette).
  La barre d'ancrage de Windows 11 qui descend au même endroit est coupée (réglage « Afficher
  les dispositions d'ancrage… en haut de l'écran », remis comme avant si on désactive la
  fonctionnalité). Windows ne relit ce réglage qu'à l'ouverture de session : pour qu'il
  s'applique tout de suite, décochez-le dans Paramètres > Système > Multitâche > Ancrer les
  fenêtres.
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
  accrochée et les fenêtres épinglées (grisées sans l'encoche), le lanceur, les coins arrondis —
  ainsi que le lancement au démarrage de Windows. Le changement est immédiat : sans les
  scènes, toutes les fenêtres
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

Au démarrage, les fenêtres ouvertes sont regroupées par application et par écran : une scène
par appli sur chaque écran.

**Plusieurs écrans** : chaque écran a son propre Stage Manager, comme sur macOS — sa pastille
d'icônes près de son encoche, ses scènes et sa scène active. Changer de scène sur un écran ne touche pas aux
autres. Une fenêtre déplacée sur un autre écran (à la souris ou avec Win + Maj + flèche) quitte
sa scène et rejoint la scène active de son nouvel écran ; les nouvelles fenêtres rejoignent les
scènes de l'écran où elles s'ouvrent. Un écran débranché confie ses scènes à l'écran principal,
un écran branché reçoit aussitôt sa barre et son encoche. Une appli plein écran n'efface que la
barre et l'encoche de son écran.

Ensuite :

- une nouvelle fenêtre d'une appli déjà présente dans la scène active la rejoint ;
- une nouvelle appli ouvre sa propre scène ;
- passer à une fenêtre par Alt+Tab ou la barre des tâches change de scène ;
- réduire une fenêtre la retire de sa scène.

Les fenêtres « toujours au premier plan » (vidéo en incrustation, fenêtre épinglée…) ne
font partie d'aucune scène : elles flottent au-dessus de toutes, comme sur macOS.

En quittant (ou en cas de plantage), toutes les fenêtres réduites par l'application sont restaurées.

## Animations

Quand on change de scène, les fenêtres sortantes rapetissent jusqu'à leur icône et les fenêtres
entrantes en jaillissent pour reprendre leur place ; les icônes glissent pour se réorganiser et
la pastille s'allonge ou se resserre avec elles. Une icône qui arrive surgit en grandissant,
l'icône survolée grossit avec un petit rebond, et s'enfonce sous le clic. Quand l'encoche
s'ouvre, la pastille remonte dans le bord de l'écran pour lui laisser la place.
Les mouvements suivent un ressort amorti (léger dépassement puis retour, comme sur macOS).
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
| `src/stages.rs` | Modèle des scènes (une scène active par écran), sans Win32, couvert par des tests (`cargo test`) |
| `src/win.rs` | Utilitaires Win32 : filtrage des fenêtres, processus, icônes, premier plan |
| `src/app.rs` | Barre d'icônes des scènes près de l'encoche, miniatures DWM, icône de notification, événements |
| `src/app/screens.rs` | Écrans : une barre, un overlay et une encoche par écran, branchements, plein écran |
| `src/anim.rs` | Animations : fenêtre transparente, trajectoires, courbes d'accélération |
| `src/canvas.rs` | Dessin avec transparence par pixel pour la barre, projection en perspective |
| `src/dock.rs` | Modèle du Dock : éléments, épinglage, grossissement, rebond (testé) |
| `src/app/dockbar.rs` | Fenêtre du Dock : place réservée, dessin, clics, menu |
| `src/shell.rs` | Raccourcis épinglés, grandes icônes, noms d'applis, lancement, corbeille |
| `src/taskbar.rs` | Masquage automatique de la barre des tâches, et sa remise en état |
| `src/snapbar.rs` | Barre d'ancrage de Windows 11 coupée, et sa remise en état |
| `src/wallpaper.rs` | Fond d'écran derrière le Dock, flouté, pour la teinte des panneaux (fil à part) |
| `src/launcher.rs` | Lanceur : recherche des applis (testée) |
| `src/app/launcherui.rs` | Fenêtre du lanceur : saisie, résultats |
| `src/app/notch.rs` | Encoche en haut de chaque écran : formes, animation, lecteur complet |
| `src/app/float.rs` | Fenêtres épinglées : glisser vers l'encoche, premier plan, vignette dans le coin |
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
`DwmRegisterThumbnail` (miniatures en direct), `SHAppBarMessage` (être prévenu des applis plein
écran), `ShowWindowAsync` (une appli figée ne bloque pas la nôtre).

## Limites connues

- Les fenêtres lancées en administrateur ne sont pas gérées (Windows l'interdit), sauf si
  Stage Manager est lui-même lancé en administrateur.
- Le Dock, le lanceur et les coins arrondis n'apparaissent que sur l'écran principal.
- L'encoche recouvre le milieu du haut de l'écran (environ 190 × 32 pixels à 100 %) : un clic
  à cet endroit l'ouvre au lieu d'atteindre la fenêtre en dessous (onglet, barre de titre).
- Les bureaux virtuels ne sont pas encore pris en compte.
- La barre montre au plus 8 scènes. Une scène qui regroupe plusieurs applis n'affiche que
  l'icône de sa fenêtre la plus récente.
- Les applis Chromium/Electron (Discord, Chrome…) cessent de se dessiner quand elles restent
  longtemps réduites : leur miniature volante peut être noire au début du trajet.
