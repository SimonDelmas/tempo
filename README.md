# Tempo

Application locale pour organiser ses notes Markdown, suivre ses tâches, traiter les résumés
de mails / Teams et générer des rapports (hebdo manager, avancement de tâche) avec l'API Vibe.

Conception détaillée : [CONCEPTION.md](CONCEPTION.md).

## Prérequis

- [Rust](https://rustup.rs/) stable (toolchain `msvc` sous Windows, avec les Build Tools C++)
- Node.js 20+ (pour compiler l'interface)
- WebView2 (présent par défaut sous Windows 10/11)
- Pour construire l'installeur : `cargo install tauri-cli --version "^2" --locked`

## Installation

Application de bureau (installeur Windows NSIS / MSI) :

```bash
cd frontend && npm install && cd ..
cd src-tauri && cargo tauri build
```

Les installeurs sont produits dans `target/release/bundle/nsis/` et `target/release/bundle/msi/`
(installation par utilisateur, sans droits administrateur pour la version NSIS). Au premier
build, la CLI Tauri télécharge NSIS et WiX depuis GitHub ; si le réseau bloque NSIS, produire
seulement le MSI avec `cargo tauri build --bundles msi`. L'exécutable seul est
`target/release/tempo.exe`.

Configuration : copier `.env.example` en `.env` et renseigner `TEMPO_VAULT`, `VIBE_BASE_URL`,
`VIBE_API_KEY` et les modèles. Le fichier est cherché, dans l'ordre, dans `TEMPO_ENV_FILE`,
le dossier courant, la racine du dépôt (développement) puis `%APPDATA%\Tempo\.env`
(application installée). Les variables d'environnement l'emportent sur les fichiers.

Sans `TEMPO_VAULT`, l'application utilise `vault-exemple/` en développement et
`Documents\Tempo` une fois installée. Sans `VIBE_*`, tout fonctionne sauf les fonctions IA
(analyse de l'inbox, rapports, chat). Les certificats du système (magasin Windows) sont
utilisés pour joindre Vibe ; `VIBE_CA_BUNDLE` permet d'imposer un fichier PEM.

## Lancement

```bash
cd frontend && npm run build && cd ..
cargo run -p tempo-app                   # application de bureau (build de développement)
cargo run -p tempo-core --bin tempo-server   # ou : serveur seul sur http://127.0.0.1:8765
```

L'application de bureau sert l'API et l'interface sur `127.0.0.1:48765` (modifiable avec
`TEMPO_DESKTOP_PORT`), protégé par un jeton propre à chaque lancement ; une seconde ouverture ramène la fenêtre existante.

Développement de l'interface avec rechargement à chaud : `cargo run -p tempo-core --bin
tempo-server` dans un terminal, `cd frontend && npm run dev` dans un autre, puis
http://localhost:5173.

## Utilisation

- **Navigation** : la barre de gauche donne accès aux espaces Notes, Tâches, Rapports,
  Imputations et Inbox (Alt+1…5), chacun avec sa propre barre latérale. Chaque page a son
  adresse (`#/taches?projet=sap`, `#/notes/reunions/…`, `#/imputations?date=…`) : retour
  arrière du navigateur et liens directs fonctionnent. Une note s'ouvre dans l'espace de son
  dossier (`taches/` → Tâches, `rapports/` → Rapports, `comptes/` et `imputations/` →
  Imputations) ; chaque espace retrouve la dernière page consultée.
- **Chat** : questions en langage naturel sur les notes et les tâches. L'agent consulte le
  vault (recherche, lecture de notes, liste et historique des tâches), cite ses sources en
  `[[liens]]` cliquables et affiche sa réponse au fil de l'eau. Il peut proposer de créer une
  tâche, de mettre à jour une tâche ou d'ajouter une ligne au journal : rien n'est écrit avant
  « Appliquer ». Les conversations sont conservées dans l'application (stockage local du navigateur intégré).
- **Recherche sémantique** (si `TEMPO_EMBED_MODEL` est renseigné) : bouton ≈ à côté de la
  recherche des notes, suggestions de liens (notes proches pas encore liées) dans le panneau
  de droite, et outil de recherche par le sens pour l'agent du chat. Les notes sont
  vectorisées en arrière-plan dans `<vault>/.tempo/vectors.db` (sqlite-vec), seulement
  quand elles changent.
- **Journal du jour** : crée / ouvre `journal/AAAA/AAAA-MM-JJ.md`.
- **Éditeur** : `[[` pour lier une note ou une tâche, `/` pour les commandes (`/tache`,
  `/date`, `/action`, `/reunion`), Ctrl+clic sur un lien pour l'ouvrir, Ctrl+P pour
  l'ouverture rapide. Sauvegarde automatique ; les modifications faites dans un autre
  éditeur sont rechargées, et un conflit est signalé si les deux versions divergent.
  Bouton **Aperçu** : aperçu en direct façon Obsidian (syntaxe Markdown masquée hors de la
  ligne en cours, puces, cases à cocher cliquables). Menu **IA** : extraire des tâches de la note (à cocher avant création, doublons signalés,
  liens ajoutés en fin de note), résumer, reformuler ou lister les actions de la sélection
  (ou de la note entière) ; le résultat est relu avant d'être inséré.
- **Tâches** : kanban par statut (glisser-déposer), fiche de tâche avec statut / priorité /
  échéance, historique daté et bouton « Rapport d'avancement ».
- **Inbox** : coller des résumés de mails / Teams (séparés par `---`) ou déposer des fichiers
  dans `00-inbox/`, « Analyser avec l'IA », ajuster la proposition, « Appliquer ».
  Le bouton « Copier le prompt de résumé » donne le format conseillé pour Copilot.
- **Rapports** : rapport hebdomadaire en 4 sections, enregistré dans `rapports/hebdo/` ;
  « Copier pour email » retire les liens sources.
- **Imputations** : comptes d'imputation (nom, alias, jours attribués) dans `comptes/`, un
  projet est rattaché à un compte via `compte: <alias>` dans son frontmatter. Saisie par
  semaine (8 h = 1 jour, chaque jour ouvré doit totaliser 8 h ; le compteur de la barre
  latérale indique les jours incomplets de la semaine en cours), enregistrée dans
  `imputations/AAAA/AAAA-Sxx.md`. « Proposer une répartition (IA) » s'appuie sur le rapport
  hebdomadaire de la semaine et sur les jours restants de chaque compte.

## Tests

```bash
cargo test
```
