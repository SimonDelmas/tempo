# Tempo — Conception

Application personnelle locale pour organiser ses notes, suivre ses tâches, traiter les
résumés de mails / Teams et générer des rapports (avancement de tâche, hebdo manager),
en s'appuyant sur l'API LLM interne Vibe (compatible OpenAI).

## Principes

1. **Les fichiers Markdown sont la source de vérité.** Le vault reste éditable avec
   n'importe quel éditeur (Obsidian, VS Code…). SQLite (`<vault>/.tempo/index.db`) n'est
   qu'un index reconstructible.
2. **Le LLM propose, l'utilisateur valide.** Aucune écriture automatique sans écran de
   validation (inbox, actions IA de l'éditeur, propositions du chat, répartition
   d'imputations) ; les rapports sont des brouillons enregistrés dans le vault.
3. **Collecte déterministe, rédaction par le LLM.** Les données d'un rapport sont
   sélectionnées par requêtes (dates, liens, statuts) ; le LLM ne fait que rédiger.
4. **Traçabilité.** Les rapports citent leurs sources en `[[wiki-links]]`.

## Architecture

Client lourd en Rust : une application de bureau Tauri 2 embarque l'interface React et le
cœur applicatif (`tempo-core`), qui expose l'API `/api/*` sur un port local.

```
 Vault Markdown  ◄── éditeurs externes
      │ notify (surveillance du dossier)
 tempo-core (crates/tempo-core/src) — bibliothèque Rust
   api.rs      routes axum /api, SSE du chat, service du frontend, jeton d'accès
   services.rs assemblage des services autour d'un vault
   config.rs   paramètres lus dans l'environnement et les fichiers .env
   vault.rs    lecture/écriture, frontmatter YAML (style PyYAML), conflits (hash), corbeille .trash/
   index.rs    index SQLite (rusqlite) : notes, liens, recherche FTS5, watcher
   tasks.rs    tâches = fichiers taches/T-XXX-*.md, historique, backlog (rang) et itérations hebdo
   journal.rs  notes quotidiennes et modèles
   inbox.rs    résumés mails/Teams → propositions → application
   reports.rs  rapport hebdo (4 sections) et rapport d'avancement de tâche
   imputations.rs  comptes d'imputation, heures par semaine, proposition de répartition, périodes de saisie
   semantic.rs embeddings des notes (sqlite-vec, .tempo/vectors.db) : recherche par le sens, notes proches
   library.rs  bibliothèque de documents : ingestion, file « À classer », thèmes, index (library.db), recherche
   convert/    conversion des documents en Markdown pivot (pandoc, pdfium, pptx, tableurs, mails, .doc / .ppt)
   assist.rs   actions IA de l'éditeur : extraction de tâches, résumé / reformulation
   drafts.rs   brouillons (.brouillons/, hors index) → proposition de note et de tâches
   chat.rs     agent : outils de consultation et de proposition, réponses citées et diffusées (SSE)
   notes.rs    modifications ciblées d'une note (passages, sections, frontmatter, liens)
   llm.rs      client OpenAI-compatible (Vibe, ureq), mode JSON, tool calling, FakeLlm pour les tests
   de.rs       désérialisation tolérante des réponses du modèle (valeurs par défaut)
   prompts.rs  prompts système (inbox, rapports, imputations, éditeur, chat)
      │ HTTP 127.0.0.1 (/api + interface)
 ┌────────────────────────────┬──────────────────────────────────────┐
 │ src-tauri : application    │ tempo-server (bin de tempo-core) :   │
 │ de bureau (fenêtre WebView2│ serveur seul sur :8765, pour le      │
 │ port 48765 + jeton)        │ développement avec Vite              │
 └────────────────────────────┴──────────────────────────────────────┘
      │
 Frontend React + Vite + CodeMirror 6 (frontend/), embarqué dans l'exécutable
```

Application de bureau : au démarrage, l'index est reconstruit, l'API est servie sur
`127.0.0.1:48765` (stable, car le stockage local de la fenêtre dépend de l'origine ;
`TEMPO_DESKTOP_PORT` pour le changer, port libre en repli) et la fenêtre ouvre `/?token=…`. Le serveur pose alors un
cookie HttpOnly / SameSite=Strict et refuse toute requête sans ce cookie : ni un autre
processus local ni une page web ne peuvent appeler l'API. Le frontend garde ses appels
relatifs `/api/*` et la diffusion SSE ; les liens externes s'ouvrent dans le navigateur
du système. Une seule instance tourne à la fois.

Frontend : chaque vue a son adresse (`#/<espace>/<note>?…`, `router.ts`). Le routeur tient
son propre historique des vues (pile + position) pour les boutons précédent / suivant, qui
savent ainsi s'ils sont actifs ; ils respectent la garde de sortie des saisies non
enregistrées. Les raccourcis viennent de `/api/config` (`TEMPO_SHORTCUT_BACK`,
`TEMPO_SHORTCUT_FORWARD`, Alt+← / Alt+→ par défaut) et sont capturés avant l'éditeur.

Les traitements (disque, SQLite, appels au LLM) sont synchrones et exécutés hors du
runtime async (`spawn_blocking`) ; la vectorisation tourne dans un thread de fond.

## Structure du vault

```
00-inbox/            résumés mails/Teams en attente
journal/AAAA/        AAAA-MM-JJ.md (sections Notes / Échanges / Fait aujourd'hui)
reunions/AAAA/       AAAA-MM-JJ-sujet.md
echanges/AAAA/MM/    échanges traités (résumé + texte original)
taches/              T-XXX-titre.md
projets/  sujets/  personnes/  ressources/  formations/
themes/              <slug>.md (thèmes des documents de la bibliothèque)
bibliotheque/        documents : pivots Markdown + sources/ (dossier configurable, hors index des notes)
rapports/hebdo/      AAAA-Sxx.md
rapports/taches/     T-XXX-AAAA-MM-JJ.md
comptes/             <alias>.md (comptes d'imputation)
imputations/AAAA/    AAAA-Sxx.md (heures saisies par jour et par compte)
imputations/saisies.md  dates de dernière saisie dans l'outil de gestion des temps
_templates/          journal.md, reunion.md, note.md
.tempo/              index.db (index), vectors.db (embeddings) : caches reconstructibles
.brouillons/         <id>.md : brouillons de l'espace Draft (frontmatter title / created /
                     updated), hors index : ni notes ni cache
.trash/              notes supprimées depuis l'application (horodatées, récupérables)
```

Frontmatter commun : `type`, `date`, `projects`, `participants`, `tasks`, `tags`.

Une note créée depuis l'application (`POST /api/notes`) porte le même nom partout : fichier
`<titre>.md` (caractères interdits par Windows ou les wiki-links remplacés par des espaces ;
réunions préfixées par la date, `AAAA-MM-JJ <titre>.md`), `title` du frontmatter et premier
titre `# …`, forcés après le rendu du modèle de `_templates/`.

Tâche :

```yaml
id: T-042
type: tache
title: Migration référentiel SAP
status: en_cours      # a_faire | en_cours | attente | bloquee | terminee
priority: haute       # basse | normale | haute
project: sap
due: 2026-10-24
rank: 3000            # ordre dans le backlog (facultatif)
iterations: [2026-S42, 2026-S43]   # semaines planifiées (facultatif)
```

avec une section `## Historique` (`- AAAA-MM-JJ : texte`) alimentée par l'application ;
c'est elle qui permet de savoir ce qui a évolué sur une période.

`project` contient l'identifiant d'une note de `projets/` (nom du fichier sans `.md`) : les
listes de projets de l'interface viennent de `GET /api/projects`, et `POST /api/projects`
(`{title}`) crée `projets/<slug>.md` (`type: projet`, `title`). Un identifiant sans note reste
accepté et s'affiche « sans note ».

### Backlog et board hebdomadaire

Inspiré d'Azure DevOps : le **backlog** liste toutes les tâches dans un seul ordre (le projet
tient lieu de feature et s'affiche en colonne) et le **board** montre les tâches d'une
itération d'une semaine (`2026-S43`, semaine ISO du lundi au dimanche, même libellé que les
imputations), chaque colonne de statut dans ce même ordre.

- `rank` (entier, croissant) ordonne toutes les tâches entre elles, quel que soit leur projet ;
  une tâche sans rang vient après les autres, par identifiant. Le backlog et le board trient
  par ce seul rang : réordonner dans l'un réordonne l'autre. `POST /api/tasks/{id}/move`
  (`{before}` ou `{after}`) place la tâche avant `before`, sinon après `after`, sinon en fin
  de backlog : rang médian entre les voisins, ou renumérotation du backlog par pas de 1000
  quand il n'y a plus de place. Le déplacement ne change ni le projet ni le statut et n'est
  pas tracé dans l'historique ; le board enchaîne `PATCH` (statut) et `move` (`before` la
  carte visée, ou `after` la dernière carte de la colonne).
- Filtres et recherche du backlog : côté interface, sur la liste déjà chargée ; un glisser
  avec filtres actifs place la tâche par rapport aux tâches affichées, dans l'ordre global.
  Le filtre projet est porté par l'adresse (`#/taches?projet=…`, `-` pour « sans projet »)
  et partagé avec le board.
- Les features gardent leur propre ordre (`rank` dans la note du projet, `projets/*.md`,
  `POST /api/projects/{id}/move` avec `{before}`), utilisé par le chat (`list_projects`) ;
  le backlog ne regroupe plus par feature. `GET /api/projects` reste trié par titre (listes
  déroulantes) et expose `rank`.
- `iterations` liste les semaines où la tâche a été planifiée ; la dernière est la semaine
  courante de planification (`iteration` dans l'API). `PATCH /api/tasks/{id}` avec
  `changes.iteration` (semaine ou `null`) remplace les semaines courantes ou futures et garde
  les semaines passées ; `POST /api/tasks` accepte `iteration`. Historique : « planifiée en
  2026-S44 », « retirée de l'itération 2026-S44 ».
- **Report** : à chaque lecture du backlog ou d'un board (`GET /api/tasks/backlog`,
  `GET /api/tasks/board?week=`), les tâches non terminées dont la dernière semaine est passée
  reçoivent la semaine courante en plus (« reportée de 2026-S42 en 2026-S43 »). Le board d'une
  semaine passée montre donc tout ce qui y était planifié, avec le statut actuel.

## Flux Inbox (mails / Teams)

1. Coller un ou plusieurs résumés (séparés par `---`) ou déposer un fichier dans `00-inbox/`.
2. Analyse LLM (JSON) avec en contexte : tâches ouvertes, projets, fils d'échanges récents.
3. Proposition : note d'échange, nouvelles tâches, mises à jour de tâches, ligne de journal.
   Les mises à jour visant une tâche inconnue sont écartées ; un résumé déjà traité est
   signalé (`duplicate_of`, via le `source_hash` des notes d'échange).
4. Validation / édition dans l'écran de tri, puis application : note
   `echanges/AAAA/MM/AAAA-MM-JJ-<source>-<sujet>.md` (résumé + section `## Texte original`),
   tâches créées / mises à jour avec un lien vers l'échange, ligne ajoutée à la section
   `## Échanges` du journal du jour de l'échange ; le fichier de `00-inbox/` est supprimé.

Format de résumé conseillé (à demander à Copilot ou équivalent) :

```
Source: mail|teams
Date: AAAA-MM-JJ
Sujet:
Participants:
Résumé:
Actions pour moi:
Actions attendues des autres:
Questions en suspens:
```

## Rapport hebdomadaire

Sections : **Réunions et échanges**, **Actions réalisées**, **Non terminé**,
**Plan pour la semaine suivante**. Sources : réunions, échanges, journal et notes de la
semaine, historique des tâches, tâches ouvertes et échéances, éléments saisis par
l'utilisateur.

## Rapport d'avancement de tâche

Enregistré dans `rapports/taches/T-XXX-AAAA-MM-JJ.md`, sections **Synthèse**, **Réalisé**,
**Blocages et risques**, **Prochaines étapes**, à partir de la fiche de la tâche (avec son
historique) et des notes qui la mentionnent (hors rapports).

## Comptes d'imputation

Compte (`comptes/sap-mig.md`) :

```yaml
type: compte
title: Migration SAP S/4
alias: SAP-MIG      # utilisable en [[SAP-MIG]]
categorie: projet   # projet | interne | conges ; projet si absente
days: 40            # jours attribués
description: Reprise des articles pour la bascule S/4   # facultative
```

La description et la catégorie sont reprises dans le contexte de la proposition de
répartition. `GET /api/accounts` trie les comptes par catégorie (projet, interne, congés) puis
par alias ; l'interface les regroupe par catégorie (barre latérale, liste, grille de saisie).

Un projet est rattaché à un compte par `compte: SAP-MIG` dans son frontmatter.

Semaine (`imputations/2026/2026-S42.md`) : frontmatter `hours` (`{date: {alias: heures}}`)
et `comment` ; le corps (tableau + justification) est régénéré à chaque enregistrement.
8 h = 1 jour ; chaque jour ouvré doit totaliser 8 h. Jours consommés = somme des heures de
tous les fichiers de semaine / 8.

Proposition de répartition : contexte = comptes (attribués, consommés, restants), projets
rattachés, heures déjà saisies, rapport hebdomadaire de la semaine (à défaut, le contexte
du rapport hebdo). Le LLM répond en JSON ; les alias inconnus sont écartés, chaque jour est
arrondi à 0,5 h et ramené à 8 h. La proposition n'est enregistrée qu'après validation.

Saisies (`imputations/saisies.md`) : frontmatter `type: saisies-imputation` et
`dates: [2026-10-04, 2026-10-11]` (dates jusqu'auxquelles les imputations ont été reportées
dans l'outil de gestion des temps) ; le corps (liste des dates) est régénéré. La première date
marque le début du suivi, chaque date suivante clôt la période commencée le lendemain de la
précédente ; la période ouverte va du lendemain de la dernière date à aujourd'hui. Pour
chaque période (`GET /api/imputations/saisies`) : bilan des heures par compte (jours, part),
complétude (jours ouvrés à 8 h) et semaines incomplètes (jours, heures manquantes ou en
trop). Le compteur de l'espace Imputations compte les jours incomplets de la période ouverte
(sans saisie : ceux de la semaine en cours). Les jours fériés et absences ne sont pas encore
distingués : les imputer sur un compte dédié.

## Actions IA de l'éditeur

Extraction de tâches d'une note (propositions à cocher, doublons signalés, liens ajoutés en
fin de note après création) ; résumé, reformulation ou liste d'actions de la sélection ou de
la note. `assist.rs` n'écrit rien : l'éditeur insère le résultat après relecture.

## Brouillons (espace Draft)

Un onglet par brouillon, fichier `.brouillons/<AAAAMMJJ-HHMMSS>.md` (ordre de création =
ordre des onglets). Le dossier commence par un point : l'index et le watcher l'ignorent,
les brouillons ne sont donc pas des notes, mais ils vivent dans le vault (source de vérité)
plutôt que dans le stockage local de la fenêtre, et survivent ainsi aux mises à jour comme
à un changement de port. Sauvegarde automatique (`PUT /api/drafts/{id}`) ; fermer un onglet
déplace le fichier dans `.trash/`.

« Analyser » (`drafts.rs`, `DRAFT_SYSTEM`) : avec les projets connus et les tâches
ouvertes en contexte, le modèle propose une note (`type` parmi `NOTE_KINDS`, titre, contenu
Markdown, `projects` limités aux projets connus) et/ou des tâches (`duplicate_of` vérifié, tâche en doublon décochée). Rien n'est
écrit avant « Créer » : la note est créée depuis son modèle (le corps remplace celui du
modèle), les tâches citent la note (`créée depuis [[…]]`), la note les liste (section
`## Tâches` et clé `tasks` si le modèle la prévoit), puis le brouillon est fermé si demandé.

## Chat

Agent en boucle d'outils (10 tours au plus) par le tool calling natif de l'API OpenAI
(`tools`, `tool_calls`, messages `tool`), géré par Vibe y compris en streaming : à chaque tour
le modèle appelle un ou plusieurs outils, ou répond en texte ; au dernier tour,
`tool_choice: none` le force à répondre. Chaque outil est décrit par un schéma JSON dont les
`enum` reprennent `STATUSES`, `PRIORITIES` et les types de notes. Outils de consultation :
`search_notes` (FTS5 sur le titre et le corps ; modes `mots`, `phrase`, `ou`),
`grep_notes` (texte exact ou regex, ligne par ligne, frontmatter compris : sert aussi à
retrouver un passage avant `propose_note_edit`), `find_notes` (morceau du nom ou du titre,
type, valeurs du frontmatter y compris dans les listes, sans casse ni accents),
`read_note`, `list_notes`, `list_tasks`, `get_task`, `task_events`, les outils de la
bibliothèque (`search_documents`, `read_document`, `list_documents`, voir Bibliothèque), `semantic_search` (si
les embeddings sont actifs) ; `read_note` donne aussi les liens sortants et entrants. La
recherche exacte se fait dans le code (`Index::grep`, `regex`), sans outil externe type
ripgrep : un vault reste assez petit pour être parcouru à chaque appel. Les modèles
(`_templates/`) sont exclus des résultats même quand leur frontmatter porte un `type`. Une
erreur que Vibe signale dans le flux SSE avec un statut 200 (quota de tokens dépassé, par
exemple) est remontée comme erreur, au lieu d'une réponse vide. Outils de proposition, sans écriture, appliqués un à un par
l'utilisateur : `propose_task`, `propose_task_update` (statut, priorité, échéance, titre,
projet, semaine), `propose_journal_line`, et pour les notes `propose_note_create` (projets
vérifiés, modifiables sur la carte avant validation),
`propose_note_edit` (passages cités, chacun unique dans la note), `propose_note_append`
(fin d'une section), `propose_note_meta` (frontmatter clé par clé ; refusé pour les champs
d'une tâche, qui passent par `propose_task_update` et son historique),
`propose_note_delete` (corbeille), `propose_link` (liste du frontmatter ou section
`## Liens`) et `propose_unlink` (entrées du frontmatter retirées, liens du corps remplacés
par leur texte). Pour le reste de l'application : consultation (`task_board`,
`task_backlog`, `list_projects`, `list_accounts`, `get_imputations`, `suggest_imputations`
— répartition proposée, sans écriture —, `imputation_tracking`, `list_inbox`, `list_drafts`)
et propositions `propose_task_move`, `propose_project_create` / `_move` / `_account`,
`propose_account_create` / `_update`, `propose_imputations` (toute la semaine remplacée,
comptes et totaux vérifiés), `propose_saisie`, `propose_inbox_add` / `_discard` /
`_process` (l'analyse habituelle de l'inbox est faite à la proposition, l'utilisateur
valide son résultat), `propose_weekly_report` et `propose_task_report` (générés à la
validation : collecte déterministe, rédaction par le LLM). Le chat reçoit ces services par
`ChatDeps`. Les brouillons restent en lecture seule pour l'agent. Les propositions sont une enum `ChatProposal` (`kind` en JSON, une variante
par opération ; l'ancien format à plat des conversations conservées reste lisible). Une
opération sur une note est vérifiée à la proposition puis rejouée à la validation sur le
contenu du moment (`notes.rs`), si bien que plusieurs propositions sur la même note se
valident dans n'importe quel ordre ; le hash ne protège que l'écriture elle-même. Le
frontmatter n'est réécrit que s'il change. Les cartes de proposition affichent un aperçu
avant / après. Le texte du modèle est diffusé en SSE (`delta`) ; s'il précède des appels
d'outils, un événement `reset` l'efface. Les conversations sont conservées côté navigateur
(texte seulement : les appels d'outils des tours précédents ne sont pas renvoyés au
modèle). Côté interface, conversations et réponses en
cours sont tenues au niveau de l'application (`useChats` dans `chats.ts`), pas dans la vue :
la diffusion survit au changement d'espace, plusieurs conversations peuvent attendre une
réponse en même temps, et une réponse terminée hors de vue marque la conversation `unread`
et déclenche une notification cliquable. Le chat n'est pas un espace routé : c'est un
panneau à droite de l'application (`ChatPanel.tsx`), toujours monté, réductible et
redimensionnable ; son état (déplié, largeur, conversation affichée) est gardé dans le
stockage local (`tempo.chatPanel`). Une conversation n'est « vue » que si le panneau est
déplié sur elle.

## Recherche sémantique

Modèle d'embeddings : celui d'une API compatible OpenAI si `TEMPO_EMBED_MODEL` est renseigné
(URL et clé de Vibe par défaut), sinon le **modèle local** livré avec l'application
(`embed_local.rs`) : multilingual-e5-small quantifié int8 (ONNX, 384 dimensions, français et
anglais, préfixes `query:` / `passage:`), exécuté par ONNX Runtime (`onnxruntime.dll`, chargée à
l'exécution par la crate `ort` en mode `load-dynamic`). Le modèle n'est chargé qu'au premier
texte à vectoriser (environ 200 Mo de mémoire) et libéré après 5 minutes sans usage ;
l'inférence se limite à 2 cœurs. Fichiers dans `resources/models/multilingual-e5-small/`
(`TEMPO_EMBED_LOCAL_DIR`, `TEMPO_ONNXRUNTIME`), téléchargés au build par
`scripts/telecharger-outils.ps1`, jamais à l'exécution ; `TEMPO_EMBED_LOCAL=off` désactive le
modèle local. La crate `ort` est figée en 2.0.0-rc.13 (Rust 1.88) : la rc.15 exige Rust 1.92 ;
l'API C d'ONNX Runtime étant rétrocompatible, la DLL 1.31 convient.
Chaque note (hors modèles et inbox) est découpée en passages d'environ 1 200 caractères
(20 au plus), vectorisés dans `.tempo/vectors.db` (sqlite-vec) en arrière-plan ; une note
n'est revectorisée que si son hash change, et un changement de modèle repart de zéro.
Usages : recherche par le sens, notes proches non liées (panneau de droite), outil du chat.

## Bibliothèque

Base documentaire gérée par l'application : les documents y sont ajoutés explicitement
(glisser-déposer depuis l'explorateur, bouton « Ajouter »), convertis en Markdown (format
pivot), classés par thèmes, projets et tags après validation, puis indexés. Volume visé :
moins d'un millier de documents, en français et en anglais, de 40 Mo au plus.

**Dossier** : `TEMPO_LIBRARY`, `<vault>/bibliotheque/` par défaut. Il est exclu de l'index et
de la surveillance des notes : ses documents ne sont ni des notes, ni des résultats de la
recherche des notes ou de `search_notes` ; la bibliothèque a son propre index.

```
bibliotheque/
  <nom>.md             pivot : frontmatter (classement, source) + texte converti
  sources/<nom>.<ext>  fichier d'origine, toujours conservé
  .trash/              documents supprimés ou remplacés (horodatés, récupérables)
```

`<nom>` vient du nom du fichier d'origine (caractères interdits remplacés, comme pour les
notes) : c'est l'identifiant du document dans l'API et dans les liens `[[<nom>]]` des notes.
Frontmatter du pivot :

```yaml
type: document
title: Spécification de l'interface SAP
themes: [interfaces]        # notes de themes/
projects: [sap]             # notes de projets/
tags: [specification]       # partagés avec les notes
summary: Décrit les flux d'articles entre…
source: sources/Spec interface SAP.docx
format: docx
source_hash: 3f2a9c…        # détection des doublons
pages: 42                   # pages d'un PDF, diapositives d'une présentation
added: 2026-10-22
attached_to: '[[Mail du 12 octobre]]'   # pièce jointe d'un mail (facultatif)
status: a_classer           # tant que le classement n'est pas validé
```

Le frontmatter porte tout ce que l'utilisateur a saisi ou validé ; le corps se régénère à
partir de la source (« Reconvertir », après une amélioration d'un convertisseur) sans toucher
au frontmatter. Le classement proposé par le LLM n'est écrit qu'après validation.

**Thèmes** : notes `themes/<slug>.md` (`type: theme`, `title`), gérées comme les projets :
`GET /api/themes`, `POST /api/themes` (`{title}`), liste déroulante avec « Nouveau thème… ».
Un document a plusieurs thèmes et plusieurs projets (ceux de `projets/`). `GET /api/tags`
liste les tags des notes et des documents, des plus fréquents aux plus rares.

**Conversion** (`convert/`), sans Python ni Office :

| Format | Conversion |
|---|---|
| md, txt | lu tel quel (frontmatter d'un Markdown retiré) |
| docx, odt, rtf, html, epub | pandoc (`TEMPO_PANDOC`, sinon livré avec l'application, sinon le PATH) |
| pptx | lecture du zip : `## Diapositive N — titre`, tableaux, notes de l'orateur |
| xlsx, xlsm, xls, ods | calamine : une section par feuille, tableaux Markdown (lignes limitées) |
| pdf | pdfium (`pdfium.dll` : `TEMPO_PDFIUM`, sinon livrée avec l'application) : texte page par page, marqueurs `<!-- page N -->` |
| eml, msg | en-têtes et corps (HTML converti par pandoc) ; les pièces jointes deviennent des documents à classer reliés au mail (`attached_to`) |
| doc, ppt | texte extrait des fichiers OLE (crate `cfb`) : paragraphes, une section par diapositive |

Images et schémas sont perdus ; les PDF scannés (sans texte) ne sont pas gérés (pas d'OCR).

**Ingestion** : `POST /api/library/upload?name=<fichier>` (corps brut, 100 Mo au plus) range
la source dans `sources/`, la convertit et crée le pivot `status: a_classer` ; un fichier
identique (même `source_hash`) est refusé. Un fichier copié à la main dans le dossier, sans
pivot qui le désigne, est listé dans la file et proposé à l'ingestion
(`POST /api/library/ingest`, `{path}` : il est alors déplacé dans `sources/`).

**File « À classer »** (`GET /api/library/queue`) : pour chaque document, le LLM
(`TEMPO_MODEL_CLASSIFY`, modèle rapide par défaut) propose titre, résumé, thèmes (parmi les
thèmes existants, plus des nouveaux thèmes suggérés), projets (existants seulement) et tags
(les existants de préférence), à partir du début du texte et de ses titres
(`POST /api/library/docs/{id}/classify`, sans écriture). L'utilisateur corrige puis valide,
document par document ou en masse (`PUT /api/library/docs/{id}`) : le frontmatter est écrit et
`status` retiré ; le même appel modifie plus tard le classement d'un document.
**Remplacement** : un dépôt dont le nom existe déjà crée un document `<nom> (2)`, proposé en
remplacement de `<nom>` (ou d'un autre document choisi sur la carte) ; validé, il en prend la
place sous le même nom (les liens restent valides) et l'ancienne source comme l'ancien pivot
partent dans `.trash/`.

**Index** (`library.db`) : cache reconstructible rangé hors du vault synchronisé, dans
`%LOCALAPPDATA%\Tempo\cache\<empreinte du dossier>\` (un fichier SQLite de cette taille réécrit
à chaque ingestion se synchronise mal). Il contient les documents (classement), leurs passages
(1 500 caractères environ, coupés aux titres de section, avec la section et la page de chacun)
et un index FTS5 sur les passages et sur la fiche (titre, tags, résumé). Il est mis à jour au
démarrage (seuls les pivots modifiés, par hash) et par un watcher du dossier. Recherche de la
vue (`GET /api/library?q=&theme=&project=&tag=&format=`) : titre, mots-clés et tags, un résultat
par document avec son extrait le plus pertinent.

**Consultation** : documents classés par thème ou par projet ; fiche du document avec le
texte converti, le PDF affiché dans l'application (`GET /api/library/docs/{id}/source`) et
« Ouvrir » avec l'application par défaut du poste (`POST /api/library/docs/{id}/open`).

**Agent** (`library_agent.rs`, outils ajoutés au chat existant) :
- `search_documents` : passages (3 au plus par document, 10 en tout) avec document, section,
  page, texte et citation prête à reprendre ; filtres thème, projet, tag, format ; sans passage
  contenant tous les mots, la recherche est élargie à l'un des mots (signalé au modèle) ;
- `read_document` : sans section ni pages, le texte entier s'il fait moins de 40 000 caractères
  (environ 10 000 tokens), sinon le résumé et la table des matières (taille et page de chaque
  section) sans texte ; avec `section` (titre, sous-sections comprises, sans casse ni accents)
  ou `pages` (`3-5`, documents PDF), cette partie, par tranches de 30 000 caractères (`offset`
  pour la suite). Le document entier n'est donc lu que lorsqu'il est court ou que l'agent le
  parcourt section par section ;
- `list_documents` : documents filtrés (thème, projet, tag, format, mots, dates d'ajout) et
  thèmes existants.

**Citations vérifiées** : l'agent cite `[[document#section]]` ou `[[document#p. N]]` (champ
`citation` des outils). Chaque passage renvoyé par `search_documents` ou `read_document` est
consigné pendant la réponse ; à la fin, chaque citation de document est vérifiée dans le code :
le document existe, la section ou la page aussi, et l'agent a lu ce passage (passage trouvé,
section ou pages lues, ou document lu en entier). La réponse (`done`) porte `citations` : lien,
document, section, page, `verified`, raison de l'échec (« passage non consulté par l'agent »,
« section introuvable dans le document », « page absente du document ») et extrait. Le chat
affiche les citations comme des liens (extrait au survol, ⚠ si non vérifiée) et la liste des
sources sous la réponse ; un clic ouvre la fiche au passage (`#/bibliotheque?doc=…&section=…`
ou `&page=…` : PDF ouvert à la page, sinon section ou page du texte surlignée).

**Recherche hybride** (`library_vectors.rs`) : chaque passage de l'index reçoit un vecteur
(même modèle que les notes, table sqlite-vec `chunk_vec` de `library.db`, même rowid que
`chunks_fts`) ; un fil de fond vectorise par lots les passages qui n'en ont pas, un document
réindexé perd ses vecteurs et un changement de modèle repart de zéro (`GET
/api/library/status` : passages vectorisés sur le total, affiché dans la barre latérale). Une
recherche combine les rangs plein texte et sémantiques par fusion RRF (1 / (60 + rang) par
liste) : `search_documents` indique pour chaque passage sa correspondance (`mots`, `sens`,
`mots+sens`) ; la vue liste d'abord les documents trouvés par les mots, puis ceux « proches par
le sens ». e5-small donne des similarités serrées (0,75 à 0,88 sur un corpus de test, hors
sujet compris) : seuls sont retenus les passages au-dessus d'un seuil propre au modèle (0,83
pour e5, `TEMPO_EMBED_MIN_SIMILARITY` pour l'ajuster au corpus réel) et à moins de 0,025 du
meilleur. Pas de reranker pour l'instant.

**Lots suivants** :
- synthèses multi-documents enregistrées comme notes, documents choisis par l'agent (écart
  assumé au principe 3) et note validée par la carte de proposition du chat.

## Modèles

`TEMPO_MODEL_FAST` (classification, analyse simple) et `TEMPO_MODEL_SMART` (rédaction des
rapports, analyse inbox) sont configurables dans `.env`.

## Roadmap

- [x] MVP : vault, index, éditeur, tâches (kanban), inbox, rapport hebdo, rapport de tâche
- [x] Embeddings (sqlite-vec) : suggestions de liens, recherche sémantique, outil du chat
- [x] Embeddings locaux (multilingual-e5-small, ONNX Runtime) quand aucune API n'est configurée
- [x] Actions IA dans l'éditeur (extraire des tâches d'une note, résumer / reformuler une sélection)
- [x] Aperçu en direct type Obsidian dans l'éditeur (syntaxe masquée hors de la ligne en cours, cases à cocher, formules `$…$` / `$$…$$` rendues par KaTeX)
- [x] Chat avec les notes en lecture seule (outils via le mode json)
- [x] Chat : actions d'écriture proposées puis validées, réponses en streaming (SSE)
- [x] Chat : réponses poursuivies en arrière-plan, notification et non-lus à la fin d'une réponse hors de vue
- [x] Chat : tool calling natif (schémas JSON des outils, appels parallèles) à la place du mode json
- [x] Chat : propositions typées (une variante par opération), outils de modification des notes et des liens, aperçu avant / après
- [x] Chat : outils pour le reste de l'application (inbox, imputations et comptes, rapports, backlog et projets ; brouillons en lecture)
- [x] Client lourd : backend réécrit en Rust (axum, rusqlite, sqlite-vec), application de bureau Tauri 2, installeurs NSIS / MSI
- [x] Imputations : dates de dernière saisie, périodes avec bilan par compte et semaines à compléter, description des comptes
- [x] Navigation vue précédente / suivante (boutons, raccourcis configurables, boutons de la souris)
- [x] Projets des tâches reliés aux notes `projets/` (liste déroulante, création d'un projet)
- [x] Liste déroulante des projets (option « Nouveau projet… ») sur tous les champs projet,
  y compris les propositions de l'inbox, du Draft et de l'extraction de tâches
- [x] Imputations : type de compte (Projet, Interne, Congés) et regroupement des comptes par type
- [x] Espace Draft : brouillons en onglets persistants, transformés en note et/ou tâches par l'IA
- [x] Tâches en backlog (ordre par glisser-déposer, projets comme features) et board par itération hebdomadaire avec report automatique
- [x] Backlog à plat : ordre unique de toutes les tâches partagé avec le board (réordonnable
  des deux côtés), feature en colonne, filtres par colonne et recherche
- [x] Chat dans un panneau latéral droit accessible depuis tous les espaces (réductible, redimensionnable, Alt+C)
- [x] Projets d'une note à sa création (clé `projects` du frontmatter) : « + Nouvelle note »,
  note proposée par le Draft et par le chat
- [x] Bibliothèque (lot 1) : dépôt de documents, conversion en Markdown pivot, file « À classer »
  (thèmes, projets, tags, résumé proposés puis validés), vue par thème ou projet, recherche
- [x] Bibliothèque (lot 2) : outils de l'agent et questions-réponses avec citations vérifiées
- [x] Bibliothèque (lot 3) : embeddings locaux et recherche hybride (RRF)
- [ ] Bibliothèque (lot 4) : synthèses multi-documents enregistrées comme notes
- [ ] Imputations : jours fériés et absences pris en compte dans la complétude
