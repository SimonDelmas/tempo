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
   tasks.rs    tâches = fichiers taches/T-XXX-*.md, historique
   journal.rs  notes quotidiennes et modèles
   inbox.rs    résumés mails/Teams → propositions → application
   reports.rs  rapport hebdo (4 sections) et rapport d'avancement de tâche
   imputations.rs  comptes d'imputation, heures par semaine, proposition de répartition, périodes de saisie
   semantic.rs embeddings des notes (sqlite-vec, .tempo/vectors.db) : recherche par le sens, notes proches
   assist.rs   actions IA de l'éditeur : extraction de tâches, résumé / reformulation
   chat.rs     agent : outils de consultation et de proposition, réponses citées et diffusées (SSE)
   llm.rs      client OpenAI-compatible (Vibe, ureq), mode JSON, FakeLlm pour les tests
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
rapports/hebdo/      AAAA-Sxx.md
rapports/taches/     T-XXX-AAAA-MM-JJ.md
comptes/             <alias>.md (comptes d'imputation)
imputations/AAAA/    AAAA-Sxx.md (heures saisies par jour et par compte)
imputations/saisies.md  dates de dernière saisie dans l'outil de gestion des temps
_templates/          journal.md, reunion.md, note.md
.tempo/              index.db (index), vectors.db (embeddings) : caches reconstructibles
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
```

avec une section `## Historique` (`- AAAA-MM-JJ : texte`) alimentée par l'application ;
c'est elle qui permet de savoir ce qui a évolué sur une période.

`project` contient l'identifiant d'une note de `projets/` (nom du fichier sans `.md`) : les
listes de projets de l'interface viennent de `GET /api/projects`, et `POST /api/projects`
(`{title}`) crée `projets/<slug>.md` (`type: projet`, `title`). Un identifiant sans note reste
accepté et s'affiche « sans note ».

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
days: 40            # jours attribués
description: Reprise des articles pour la bascule S/4   # facultative
```

La description est reprise dans le contexte de la proposition de répartition.

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

## Chat

Agent en boucle d'outils (8 étapes au plus) via le mode JSON plutôt que le tool calling
natif : à chaque étape le modèle demande un outil ou donne sa réponse. Outils de
consultation : `search_notes`, `read_note`, `list_tasks`, `semantic_search` (si les
embeddings sont actifs). Outils de proposition, sans écriture : `propose_task`,
`propose_task_update`, `propose_journal_line`, appliqués un à un par l'utilisateur.
La réponse finale est diffusée en SSE (champ `answer` extrait du JSON partiel) ; les
conversations sont conservées côté navigateur.

## Recherche sémantique

Activée par `TEMPO_EMBED_MODEL` (API compatible OpenAI, URL et clé de Vibe par défaut).
Chaque note (hors modèles et inbox) est découpée en passages d'environ 1 200 caractères
(20 au plus), vectorisés dans `.tempo/vectors.db` (sqlite-vec) en arrière-plan ; une note
n'est revectorisée que si son hash change, et un changement de modèle repart de zéro.
Usages : recherche par le sens, notes proches non liées (panneau de droite), outil du chat.

## Modèles

`TEMPO_MODEL_FAST` (classification, analyse simple) et `TEMPO_MODEL_SMART` (rédaction des
rapports, analyse inbox) sont configurables dans `.env`.

## Roadmap

- [x] MVP : vault, index, éditeur, tâches (kanban), inbox, rapport hebdo, rapport de tâche
- [x] Embeddings (sqlite-vec) : suggestions de liens, recherche sémantique, outil du chat
- [x] Actions IA dans l'éditeur (extraire des tâches d'une note, résumer / reformuler une sélection)
- [x] Aperçu en direct type Obsidian dans l'éditeur (syntaxe masquée hors de la ligne en cours, cases à cocher)
- [x] Chat avec les notes en lecture seule (outils via le mode json)
- [x] Chat : actions d'écriture proposées puis validées, réponses en streaming (SSE)
- [x] Client lourd : backend réécrit en Rust (axum, rusqlite, sqlite-vec), application de bureau Tauri 2, installeurs NSIS / MSI
- [x] Imputations : dates de dernière saisie, périodes avec bilan par compte et semaines à compléter, description des comptes
- [x] Navigation vue précédente / suivante (boutons, raccourcis configurables, boutons de la souris)
- [x] Projets des tâches reliés aux notes `projets/` (liste déroulante, création d'un projet)
- [ ] Imputations : jours fériés et absences pris en compte dans la complétude
