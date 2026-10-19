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

```
 Vault Markdown  ◄── éditeurs externes
      │ watchdog
 Backend FastAPI (backend/tempo)
   main.py     routes REST /api, service du frontend compilé
   config.py   paramètres lus dans .env
   vault.py    lecture/écriture, frontmatter, détection de conflits (hash), corbeille .trash/
   index.py    index SQLite : notes, liens, recherche FTS5, watcher
   tasks.py    tâches = fichiers taches/T-XXX-*.md, historique
   journal.py  notes quotidiennes et modèles
   inbox.py    résumés mails/Teams → propositions → application
   reports.py  rapport hebdo (4 sections) et rapport d'avancement de tâche
   imputations.py  comptes d'imputation, heures par semaine, proposition de répartition
   semantic.py embeddings des notes (sqlite-vec, .tempo/vectors.db) : recherche par le sens, notes proches
   assist.py   actions IA de l'éditeur : extraction de tâches, résumé / reformulation
   chat.py     agent : outils de consultation et de proposition, réponses citées et diffusées (SSE)
   llm.py      client OpenAI-compatible (Vibe), mode JSON, FakeLLM pour les tests
   prompts.py  prompts système (inbox, rapports, imputations, éditeur, chat)
      │ REST /api
 Frontend React + Vite + CodeMirror 6 (frontend/)
```

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
_templates/          journal.md, reunion.md, note.md
.tempo/              index.db (index), vectors.db (embeddings) : caches reconstructibles
.trash/              notes supprimées depuis l'application (horodatées, récupérables)
```

Frontmatter commun : `type`, `date`, `projects`, `participants`, `tasks`, `tags`.

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
```

Un projet est rattaché à un compte par `compte: SAP-MIG` dans son frontmatter.

Semaine (`imputations/2026/2026-S42.md`) : frontmatter `hours` (`{date: {alias: heures}}`)
et `comment` ; le corps (tableau + justification) est régénéré à chaque enregistrement.
8 h = 1 jour ; chaque jour ouvré doit totaliser 8 h. Jours consommés = somme des heures de
tous les fichiers de semaine / 8.

Proposition de répartition : contexte = comptes (attribués, consommés, restants), projets
rattachés, heures déjà saisies, rapport hebdomadaire de la semaine (à défaut, le contexte
du rapport hebdo). Le LLM répond en JSON ; les alias inconnus sont écartés, chaque jour est
arrondi à 0,5 h et ramené à 8 h. La proposition n'est enregistrée qu'après validation.

## Actions IA de l'éditeur

Extraction de tâches d'une note (propositions à cocher, doublons signalés, liens ajoutés en
fin de note après création) ; résumé, reformulation ou liste d'actions de la sélection ou de
la note. `assist.py` n'écrit rien : l'éditeur insère le résultat après relecture.

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
