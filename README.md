# Tempo

Application locale pour organiser ses notes Markdown, suivre ses tâches, traiter les résumés
de mails / Teams et générer des rapports (hebdo manager, avancement de tâche) avec l'API Vibe.

Conception détaillée : [CONCEPTION.md](CONCEPTION.md).

## Prérequis

- [uv](https://docs.astral.sh/uv/) (installe Python 3.12+ automatiquement)
- Node.js 20+ (uniquement pour compiler l'interface)

## Installation

```bash
uv sync
cd frontend && npm install && npm run build && cd ..
cp .env.example .env   # puis renseigner TEMPO_VAULT, VIBE_BASE_URL, VIBE_API_KEY, modèles
```

Sans `TEMPO_VAULT`, l'application utilise `vault-exemple/`. Sans `VIBE_*`, tout fonctionne
sauf les fonctions IA (analyse de l'inbox, rapports).

## Lancement

```bash
uv run tempo
```

Puis ouvrir http://127.0.0.1:8765.

Développement de l'interface avec rechargement à chaud : `uv run tempo` dans un terminal,
`cd frontend && npm run dev` dans un autre, puis http://localhost:5173.

## Utilisation

- **Navigation** : la barre de gauche donne accès aux espaces Notes, Tâches, Rapports,
  Imputations et Inbox (Alt+1…5), chacun avec sa propre barre latérale. Chaque page a son
  adresse (`#/taches?projet=sap`, `#/notes/reunions/…`, `#/imputations?date=…`) : retour
  arrière du navigateur et liens directs fonctionnent. Une note s'ouvre dans l'espace de son
  dossier (`taches/` → Tâches, `rapports/` → Rapports, `comptes/` et `imputations/` →
  Imputations) ; chaque espace retrouve la dernière page consultée.
- **Journal du jour** : crée / ouvre `journal/AAAA/AAAA-MM-JJ.md`.
- **Éditeur** : `[[` pour lier une note ou une tâche, `/` pour les commandes (`/tache`,
  `/date`, `/action`, `/reunion`), Ctrl+clic sur un lien pour l'ouvrir, Ctrl+P pour
  l'ouverture rapide. Sauvegarde automatique ; les modifications faites dans un autre
  éditeur sont rechargées, et un conflit est signalé si les deux versions divergent.
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
uv run pytest
```
