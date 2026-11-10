# Rewards Autoplay

Application Windows (Rust + Tauri 2) qui suit vos points Microsoft Rewards et vous rappelle chaque jour ce qu'il reste à faire, y compris les quêtes Xbox Game Pass. Elle tourne en arrière-plan dans la zone de notification et se met à jour toute seule.

## Fonctionnalités (v0.1)

- Connexion au compte Microsoft sur la page officielle de Microsoft Rewards, dans une fenêtre dédiée. L'application ne voit jamais le mot de passe.
- Tableau de bord : solde de points, niveau, recherches Bing du jour, activités du jour, cartes à poinçonner et autres activités.
- Séries (Recherche Bing, série quotidienne, application Bing) avec la frise des 7 jours, l'état du jour et la carte des 12 tampons bonus.
- Quêtes suivies : une étoile sur chaque quête pour la retrouver, avec toutes ses étapes, dans une page dédiée.
- Actualisation automatique des points, séries et quêtes toutes les N minutes (réglable, 30 par défaut).
- Récompenses par jeu : recherche d'un jeu (ex. Sea of Thieves) pour voir les quêtes en cours qui le concernent (cartes à poinçonner avec leurs étapes, lien vers la page de la quête) et les articles du catalogue d'échange (prix, points manquants).
- Quêtes Xbox réelles (expérimental) : connexion Xbox Live avec l'inscription d'application « Rewards Autoplay », puis lecture du Rewards Hub de l'app Xbox (`xgrant.xboxlive.com/…/programInfo`, format documenté par le projet xau-mobile) : quêtes du jour, bonus hebdomadaires PC et console, application mobile, packs et points à réclamer. Le jeton de rafraîchissement est rangé dans le gestionnaire d'identifiants Windows.
- Suivi manuel des quêtes Xbox (sans connexion Xbox Live) : jeu quotidien coché à la main, bonus hebdomadaire (5 jours), jeux différents du mois pour les packs 4 et 8 jeux.
- Mode **Rappels** : une notification par jour à l'heure choisie, avec ce qu'il reste à faire, dont les séries dont l'étape du jour n'est pas faite.
- Zone de notification : fermer la fenêtre ne quitte pas l'application. Démarrage avec Windows en option.
- Mise à jour depuis l'application (Paramètres › Mises à jour, ou menu de l'icône), avec recherche automatique à intervalle réglable et installation automatique en option.

Le mode **Automatique** (lancement des jeux installés) n'est pas encore disponible. Le règlement Microsoft Rewards interdit les outils automatisés pour gagner des points : un compte qui l'utilise risque une suspension.

## Limites connues

- Les points, activités et quêtes sont lus dans les données intégrées aux pages rewards.bing.com/earn et /dashboard (format Next.js non documenté). Si Microsoft change ces pages, l'affichage cassera : les dernières pages lues sont enregistrées dans le dossier des journaux (Paramètres › Ouvrir le dossier des journaux) pour diagnostiquer.
- Le catalogue d'échange est lu dans les données intégrées à la page rewards.bing.com/redeem/shop (dernière page enregistrée dans le dossier des journaux). Une refonte du site peut le casser.
- L'avancement des quêtes Xbox n'est pas publié par Microsoft : le suivi Xbox est déclaratif, et les points se réclament dans le Rewards Hub de l'app Xbox.

## Développement

Prérequis : Rust (toolchain MSVC), Visual Studio Build Tools, WebView2 (inclus dans Windows 11), `cargo install tauri-cli --version "^2"`.

```powershell
cargo tauri dev        # lance l'application
cargo test --workspace # tests
cargo tauri build      # installateur NSIS dans target/release/bundle/nsis
```

Depuis WSL, utiliser la toolchain Windows (`cargo.exe`) avec un dossier de compilation côté Windows : écrire `target/` sur le disque WSL échoue (« Accès refusé »).

```bash
export CARGO_TARGET_DIR='C:\Users\powange\.cache\rewards-autoplay-target' WSLENV=CARGO_TARGET_DIR
cargo.exe tauri build
```

Structure :

| Chemin | Rôle |
|---|---|
| `src-tauri/src/lib.rs` | Démarrage, plugins, commandes appelées par l'interface |
| `src-tauri/src/rewards.rs` | Lecture du tableau de bord Rewards (points, activités, quêtes) |
| `src-tauri/src/xbox.rs` | Connexion Xbox Live et quêtes du Rewards Hub de l'app Xbox (expérimental) |
| `src-tauri/src/streaks.rs` | Séries, tampons bonus et compteur de série |
| `src-tauri/src/flight.rs` | Extraction des données intégrées aux pages Next.js du site Rewards |
| `src-tauri/src/catalog.rs` | Catalogue d'échange, lu dans la page rewards.bing.com/redeem/shop |
| `src-tauri/src/auth.rs` | Fenêtre de connexion Microsoft, déconnexion |
| `src-tauri/src/store.rs` | Réglages et historique Xbox (`state.json`) |
| `src-tauri/src/reminders.rs` | Rappel quotidien et vérification des mises à jour |
| `src-tauri/src/tray.rs` | Icône et menu de la zone de notification |
| `src-tauri/src/updater.rs` | Recherche et installation des mises à jour |
| `ui/` | Interface (HTML, CSS, JavaScript sans bundler) |

## Publier une version

Les mises à jour sont lues sur `https://github.com/powange/microsoft-rewards/releases/latest/download/latest.json`. Le dépôt doit être **public** pour que l'application puisse télécharger les releases.

1. Une seule fois : ajouter le secret GitHub `TAURI_SIGNING_PRIVATE_KEY` avec le contenu de `C:\Users\powange\.tauri\rewards-autoplay.key` (clé sans mot de passe ; sinon ajouter aussi `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`). La clé publique correspondante est dans `src-tauri/tauri.conf.json`. Ne jamais committer la clé privée : si elle est perdue, les versions installées ne pourront plus se mettre à jour.
2. Mettre le même numéro de version dans `src-tauri/Cargo.toml` et `src-tauri/tauri.conf.json`.
3. Créer et pousser le tag : `git tag v0.1.1 && git push origin v0.1.1`.

Le workflow `.github/workflows/build-release.yml` vérifie le code, construit l'installateur signé pour l'updater, génère `latest.json` et publie la release.
