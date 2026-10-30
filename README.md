# Stage Manager pour Windows

Une reproduction du Stage Manager de macOS pour Windows 11, écrite en Rust.

Une barre transparente sur le bord gauche de l'écran affiche les autres « scènes » sous forme
de miniatures en direct, posées sur le fond d'écran. La scène active reste seule à l'écran ;
les autres sont réduites.

## Utilisation

```bash
cargo run --release
```

- **Clic** sur une miniature : affiche cette scène et range la scène active dans la barre.
- **Glisser une miniature hors de la barre** (ou Maj + clic, ou clic droit → « Ajouter à la
  scène active ») : regroupe les fenêtres de cette scène avec la scène active. La relâcher
  sur la barre annule.
- **Ctrl + Alt + S** : active ou désactive le Stage Manager (toutes les fenêtres sont restaurées).
- **Icône dans la zone de notification** (clic droit) : activer/désactiver, lancer au
  démarrage de Windows, quitter. Windows 11 la range d'abord dans les icônes cachées (^) :
  glissez-la dans la barre des tâches pour la garder visible.

### Démarrage avec Windows

L'option du menu enregistre l'exécutable dans la clé `Run` de l'utilisateur
(`HKCU\Software\Microsoft\Windows\CurrentVersion\Run`, valeur `StageManager`). Si
l'exécutable change de place, la version release met la clé à jour à son lancement.
Attention : `cargo clean` supprime `target\release\stage-manager.exe` ; il faudra recompiler
pour que le lancement automatique fonctionne de nouveau.

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
| `src/canvas.rs` | Dessin avec transparence par pixel pour la barre (fenêtre en couches) |
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
- Les applis Chromium/Electron (Discord, Chrome…) cessent de se dessiner quand elles restent
  longtemps réduites : leur miniature peut alors être noire jusqu'à leur réaffichage.
