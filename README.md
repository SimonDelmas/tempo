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
  Imputations, Inbox, Chat et Draft (Alt+1…7), chacun avec sa propre barre latérale. Chaque page a son
  adresse (`#/taches?projet=sap`, `#/notes/reunions/…`, `#/imputations?date=…`) : retour
  arrière du navigateur et liens directs fonctionnent. Une note s'ouvre dans l'espace de son
  dossier (`taches/` → Tâches, `rapports/` → Rapports, `comptes/` et `imputations/` →
  Imputations) ; chaque espace retrouve la dernière page consultée. Les flèches en haut de
  la barre (Alt+← / Alt+→, ou les boutons latéraux de la souris) reviennent à la vue
  précédente et passent à la suivante ; raccourcis modifiables avec `TEMPO_SHORTCUT_BACK` et
  `TEMPO_SHORTCUT_FORWARD` (ex. `Ctrl+Alt+ArrowLeft`, voir `.env.example`).
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
- **Arborescence** : les dossiers sont pliés par défaut, ceux de la note ouverte se
  déplient ; le bouton ⊞ / ⊟ à côté de la recherche déplie ou plie tout.
- **Éditeur** : `[[` pour lier une note ou une tâche, `/` pour les commandes (`/tache`,
  `/date`, `/action`, `/reunion`), Ctrl+clic sur un lien pour l'ouvrir, Ctrl+P pour
  l'ouverture rapide. Sauvegarde automatique ; les modifications faites dans un autre
  éditeur sont rechargées, et un conflit est signalé si les deux versions divergent.
  Bouton **Aperçu** : aperçu en direct façon Obsidian (syntaxe Markdown masquée hors de la
  ligne en cours, puces, cases à cocher cliquables, formules
  mathématiques `$0.2 m/s$` ou `$$…$$` rendues par KaTeX). Menu **IA** : extraire des tâches de la note (à cocher avant création, doublons signalés,
  liens ajoutés en fin de note), résumer, reformuler ou lister les actions de la sélection
  (ou de la note entière) ; le résultat est relu avant d'être inséré.
- **Tâches** : deux onglets inspirés d'Azure DevOps.
  - **Backlog** : les tâches regroupées par projet (les projets tiennent lieu de features),
    ordonnées par glisser-déposer ; déposer une tâche sous un autre projet la rattache à ce
    projet. Les features elles-mêmes s'ordonnent en glissant leur ligne d'en-tête. La colonne Itération planifie une tâche sur une semaine (courante ou suivantes).
  - **Board** : kanban par statut d'une semaine (glisser une carte dans une autre colonne
    change son statut), la semaine courante par défaut ; flèches et liste pour afficher les autres semaines. Une tâche créée depuis le board
    est planifiée sur la semaine affichée. Au changement de semaine, les tâches non terminées
    basculent dans la semaine courante (« reportée ») et restent visibles sur le board de la
    semaine quittée.

  Fiche de tâche avec statut / priorité / échéance / projet, historique daté et bouton
  « Rapport d'avancement ». Partout où un projet est demandé (fiche de tâche, création de
  tâche, propositions de l'inbox, du Draft et de l'extraction de tâches), il se choisit dans la
  liste des notes de `projets/` ; l'option « + Nouveau projet… » de cette liste crée la note
  d'un nouveau projet et le sélectionne.
- **Inbox** : coller des résumés de mails / Teams (séparés par `---`) ou déposer des fichiers
  dans `00-inbox/`, « Analyser avec l'IA », ajuster la proposition, « Appliquer ».
  Appliquer ou ignorer un entrant passe au suivant ; l'échange est ouvert une fois l'inbox vidée.
  Le bouton « Copier le prompt de résumé » donne le format conseillé pour Copilot.
- **Draft** : brouillons en onglets pour écrire au fil de l'eau (« + » pour en ouvrir un,
  « × » ou clic molette pour le fermer, titre facultatif). Ce ne sont pas des notes : ils
  sont enregistrés automatiquement dans `<vault>/.brouillons/`, hors de l'index, et
  retrouvés au redémarrage comme après une mise à jour. « Analyser » demande à l'IA de
  transformer le brouillon en note (type, titre et contenu mis en forme) et/ou en tâches
  (doublons de tâches ouvertes signalés) ; après ajustement, « Créer » écrit la note et les
  tâches (reliées entre elles) puis ferme le brouillon si la case est cochée.
- **Rapports** : rapport hebdomadaire en 4 sections, enregistré dans `rapports/hebdo/` ;
  « Copier pour email » retire les liens sources.
- **Imputations** : comptes d'imputation (nom, alias, type, jours attribués, description
  facultative) dans `comptes/` ; le type (Projet, Interne ou Congés) classe les comptes dans la
  barre latérale, la liste des comptes et la grille de saisie. Un projet est rattaché à un compte via `compte: <alias>` dans
  son frontmatter. Saisie par semaine (8 h = 1 jour, chaque jour ouvré doit totaliser 8 h),
  enregistrée dans `imputations/AAAA/AAAA-Sxx.md`. « Suivi des saisies » : renseignez la date
  jusqu'à laquelle vos imputations sont reportées dans l'outil de gestion des temps
  (`imputations/saisies.md`) ; ces dates découpent des périodes, et pour chacune Tempo affiche
  le bilan par compte (heures, jours, part), la complétude et les semaines à compléter (un
  clic ouvre la semaine). Le compteur de l'espace indique les jours incomplets depuis la
  dernière saisie (à défaut, dans la semaine en cours). « Proposer une répartition (IA) » s'appuie sur le rapport
  hebdomadaire de la semaine et sur les jours restants de chaque compte.

## Tests

```bash
cargo test
```
