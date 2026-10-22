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
powershell -ExecutionPolicy Bypass -File scripts/telecharger-outils.ps1
cd src-tauri && cargo tauri build
```

Le script télécharge dans `src-tauri/resources/` les outils de conversion de la bibliothèque
(`pandoc.exe`, `pdfium.dll`), livrés avec l'installeur ; en développement, ils y sont aussi
cherchés (puis dans le PATH).

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
  Imputations, Inbox, Draft et Bibliothèque (Alt+1…7), chacun avec sa propre barre latérale. Chaque page a son
  adresse (`#/taches?projet=sap`, `#/notes/reunions/…`, `#/imputations?date=…`) : retour
  arrière du navigateur et liens directs fonctionnent. Une note s'ouvre dans l'espace de son
  dossier (`taches/` → Tâches, `rapports/` → Rapports, `comptes/` et `imputations/` →
  Imputations) ; chaque espace retrouve la dernière page consultée. Les flèches en haut de
  la barre (Alt+← / Alt+→, ou les boutons latéraux de la souris) reviennent à la vue
  précédente et passent à la suivante ; raccourcis modifiables avec `TEMPO_SHORTCUT_BACK` et
  `TEMPO_SHORTCUT_FORWARD` (ex. `Ctrl+Alt+ArrowLeft`, voir `.env.example`).
- **Chat** : panneau à droite de l'application, accessible depuis tous les espaces. Il se
  réduit en une barre étroite (bouton », Alt+C ; Alt+C le déplie et place le curseur dans la
  saisie) et se redimensionne en glissant son bord gauche (double-clic : largeur par défaut) ;
  largeur, état et conversation affichée sont mémorisés. Le bouton ☰ liste les conversations,
  + en démarre une nouvelle. Questions en langage naturel sur les notes et les tâches. L'agent consulte le
  vault (recherche par mots ou par phrase, recherche exacte ou par expression régulière,
  recherche par nom de note ou par propriété du frontmatter — ex. les réunions où une personne
  était présente —, lecture de notes, liste et historique des tâches), cite ses sources en
  `[[liens]]` cliquables et affiche sa réponse au fil de l'eau. Il interroge aussi la bibliothèque :
  recherche de passages, lecture d'une section, de pages ou d'un document court, liste par thème ou
  projet ; chaque information tirée d'un document est citée (`[[document#section]]`,
  `[[document#p. 12]]`) et la citation vérifiée (le passage existe et l'agent l'a lu) : un clic ouvre le
  document au passage, l'extrait s'affiche au survol, une citation non vérifiée est marquée ⚠ et
  la liste des sources figure sous la réponse. Il peut proposer de créer ou
  mettre à jour une tâche (statut, titre, projet, semaine), d'ajouter une ligne au journal, de
  créer, modifier (passages, ajout dans une section, propriétés du frontmatter) ou supprimer
  une note, et d'ajouter ou retirer des liens entre notes. Il consulte aussi le board, le
  backlog, les projets, les comptes et les imputations, l'inbox et les brouillons, et peut
  proposer de réordonner le backlog (tâches et projets), de créer un projet ou un compte, de
  rattacher un projet à un compte, de saisir les heures d'une semaine (à partir de la
  répartition suggérée), d'enregistrer une date de saisie, d'ajouter, traiter ou écarter un
  élément de l'inbox, et de générer un rapport hebdomadaire ou de tâche. Chaque proposition s'affiche avec un
  aperçu avant / après ; rien n'est écrit avant « Appliquer », et une proposition validée
  s'applique au contenu du moment (plusieurs propositions sur la même note se valident l'une
  après l'autre). Les conversations sont conservées dans l'application (stockage local du navigateur intégré).
  Une réponse continue en arrière-plan quand on change de conversation ou réduit le panneau (point animé
  sur le panneau réduit) ; si elle se termine hors de vue, une notification cliquable
  l'annonce et la conversation est marquée non lue (pastille dans la liste, compteur sur le panneau réduit).
- **Recherche sémantique** (modèle local multilingual-e5-small livré avec l'application, ou
  l'API désignée par `TEMPO_EMBED_MODEL` ; le modèle local n'est chargé qu'à l'usage et libéré
  après 5 minutes d'inactivité, `TEMPO_EMBED_LOCAL=off` le désactive) : dans la bibliothèque, la
  recherche combine les mots et le sens (documents « proches par le sens » listés à part, passages
  marqués pour l'agent) ; pour les notes, bouton ≈ à côté de la
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
  - **Backlog** : toutes les tâches dans un seul tableau, ordonnées entre elles par
    glisser-déposer. La colonne Feature montre le projet de la tâche (les projets tiennent
    lieu de features) et permet d'en changer ; la colonne Itération planifie une tâche sur une
    semaine (courante ou suivantes). Une ligne de filtres sous les en-têtes filtre chaque
    colonne (feature, statut, priorité, itération, échéance) et recherche dans les
    identifiants, titres, features et itérations ; « Effacer les filtres » les retire. Le
    filtre Feature est le même que celui de l'en-tête de l'espace (option « Sans projet »).
  - **Board** : kanban par statut d'une semaine, la semaine courante par défaut ; flèches et
    liste pour afficher les autres semaines. Les cartes suivent l'ordre du backlog : glisser
    une carte devant une autre (ou en bas d'une colonne) la replace dans cet ordre, et la
    déposer dans une autre colonne change aussi son statut. Réordonner dans le backlog
    réordonne le board, et inversement. Une tâche créée depuis le board
    est planifiée sur la semaine affichée. Au changement de semaine, les tâches non terminées
    basculent dans la semaine courante (« reportée ») et restent visibles sur le board de la
    semaine quittée.

  Fiche de tâche avec statut / priorité / échéance / projet, historique daté et bouton
  « Rapport d'avancement ». Partout où un projet est demandé (fiche de tâche, création de
  tâche, propositions de l'inbox, du Draft et de l'extraction de tâches, création d'une note
  depuis « + Nouvelle note », le Draft ou une proposition du chat), il se choisit dans la
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
  transformer le brouillon en note (type, titre, projets et contenu mis en forme) et/ou en tâches
  (doublons de tâches ouvertes signalés) ; après ajustement, « Créer » écrit la note et les
  tâches (reliées entre elles) puis ferme le brouillon si la case est cochée.
- **Bibliothèque** : base de documents (PDF, Word, PowerPoint, Excel, mails .eml / .msg, HTML,
  Markdown, y compris les .doc / .ppt d'Office 97-2003). Glisser des fichiers depuis l'explorateur
  (ou « Ajouter des fichiers… ») les convertit en Markdown et les place dans la file « À classer » :
  l'IA y propose titre, résumé, thèmes (existants, ou nouveaux thèmes à créer d'un clic), projets et
  tags, à corriger puis valider document par document ou en masse. Un fichier déjà présent est
  refusé ; un fichier du même nom est proposé comme nouvelle version (l'ancienne part dans
  `.trash/`) ; les pièces jointes d'un mail deviennent des documents reliés ; un fichier copié à la
  main dans le dossier est proposé à l'ajout. Les documents se parcourent par thème ou par projet
  et se recherchent par titre, mot-clé ou tag (extrait, section et page). La fiche montre le texte
  converti (sommaire), le PDF d'origine, et permet d'ouvrir le fichier, de modifier le classement
  ou de reconvertir. Un lien `[[nom du document]]` dans une note ouvre sa fiche. Dossier :
  `<vault>/bibliotheque/` (`TEMPO_LIBRARY`), pivots `<nom>.md` et fichiers d'origine dans
  `sources/` ; les thèmes sont des notes de `themes/`.
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
