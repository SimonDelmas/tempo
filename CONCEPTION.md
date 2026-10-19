# Tempo — Conception

Application personnelle locale pour organiser ses notes, suivre ses tâches, traiter les
résumés de mails / Teams et générer des rapports (avancement de tâche, hebdo manager),
en s'appuyant sur l'API LLM interne Vibe (compatible OpenAI).

## Principes

1. **Les fichiers Markdown sont la source de vérité.** Le vault reste éditable avec
   n'importe quel éditeur (Obsidian, VS Code…). SQLite (`<vault>/.tempo/index.db`) n'est
   qu'un index reconstructible.
2. **Le LLM propose, l'utilisateur valide.** Aucune écriture automatique sans écran de
   validation (inbox) ; les rapports sont des brouillons enregistrés dans le vault.
3. **Collecte déterministe, rédaction par le LLM.** Les données d'un rapport sont
   sélectionnées par requêtes (dates, liens, statuts) ; le LLM ne fait que rédiger.
4. **Traçabilité.** Les rapports citent leurs sources en `[[wiki-links]]`.

## Architecture

```
 Vault Markdown  ◄── éditeurs externes
      │ watchdog
 Backend FastAPI (backend/tempo)
   vault.py    lecture/écriture, frontmatter, détection de conflits (hash)
   index.py    index SQLite : notes, liens, recherche FTS5, watcher
   tasks.py    tâches = fichiers taches/T-XXX-*.md, historique
   journal.py  notes quotidiennes et modèles
   inbox.py    résumés mails/Teams → propositions → application
   reports.py  rapport hebdo (4 sections) et rapport d'avancement de tâche
   imputations.py  comptes d'imputation, heures par semaine, proposition de répartition
   llm.py      client OpenAI-compatible (Vibe), mode JSON
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
4. Validation / édition dans l'écran de tri, puis application (écriture des fichiers).

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

## Modèles

`TEMPO_MODEL_FAST` (classification, analyse simple) et `TEMPO_MODEL_SMART` (rédaction des
rapports, analyse inbox) sont configurables dans `.env`.

## Roadmap

- [x] MVP : vault, index, éditeur, tâches (kanban), inbox, rapport hebdo, rapport de tâche
- [ ] Embeddings (sqlite-vec) : suggestions de liens, recherche sémantique
- [ ] Actions IA dans l'éditeur (extraire des tâches d'une note, résumer une sélection)
- [ ] Aperçu en direct type Obsidian dans l'éditeur
- [ ] Chat avec les notes (tool calling)
