# CLAUDE.md

Tempo : application locale (FastAPI + React) pour notes Markdown, tâches, inbox mails/Teams,
rapports et imputations, assistée par l'API LLM interne Vibe (compatible OpenAI).
Fonctionnalités : [README.md](README.md). Architecture, formats du vault et roadmap :
[CONCEPTION.md](CONCEPTION.md) — à lire avant toute évolution, et à tenir à jour.

## Commandes

```bash
uv sync                          # dépendances Python (3.12+)
uv run tempo                     # serveur sur http://127.0.0.1:8765 (sert frontend/dist)
uv run pytest                    # tests backend (backend/tests)
uv run pytest backend/tests/test_tasks.py -k nom   # un test
cd frontend && npm run build     # compile l'interface (tsc -b + vite build)
cd frontend && npm run typecheck # vérification TypeScript seule
cd frontend && npm run dev       # Vite sur :5173, proxy /api → :8765 (lancer uv run tempo à côté)
```

Plateforme de dev : Windows. Pas de linter configuré.

## Principes à respecter

1. **Les fichiers Markdown du vault sont la source de vérité.** SQLite
   (`<vault>/.tempo/index.db`, `vectors.db`) n'est qu'un cache reconstructible ; ne jamais y
   stocker de donnée qui n'existe pas dans un fichier.
2. **Le LLM propose, l'utilisateur valide.** Aucune écriture dans le vault déclenchée par le
   LLM sans écran de validation (inbox, actions IA de l'éditeur, propositions du chat,
   répartition d'imputations).
3. **Collecte déterministe, rédaction par le LLM** : les données d'un rapport sont choisies
   par requêtes (dates, liens, statuts), le LLM ne fait que rédiger.
4. **Traçabilité** : les contenus générés citent leurs sources en `[[wiki-links]]`.

## Backend (`backend/tempo/`)

- `main.py` : `create_app(settings, llm, ...)` — toutes les routes `/api/*`, modèles Pydantic
  des requêtes, service de `frontend/dist`. Un service par domaine (`tasks.py`, `inbox.py`,
  `reports.py`, `imputations.py`, `assist.py`, `chat.py`, `semantic.py`…).
- `vault.py` : lecture/écriture avec frontmatter YAML et détection de conflit par hash
  (`expected_hash`) — passer par lui pour toute écriture.
- `index.py` : index SQLite (notes, liens, FTS5) + watcher watchdog.
- `llm.py` : client OpenAI-compatible, mode JSON (`extract_json`, `ensure_json_hint` : le mot
  « json » doit figurer dans un message utilisateur, pas seulement système), et `FakeLLM`
  pour les tests.
- `prompts.py` : prompts centralisés.
- `config.py` : `Settings` chargé depuis `.env` (`TEMPO_VAULT`, `VIBE_*`, `TEMPO_MODEL_FAST`,
  `TEMPO_MODEL_SMART`, `TEMPO_EMBED_*`). Sans `VIBE_*`, les fonctions IA lèvent
  `LLMNotConfigured` mais le reste fonctionne ; sans `TEMPO_EMBED_MODEL`, la recherche
  sémantique est désactivée.

## Frontend (`frontend/src/`)

React 19 + Vite + CodeMirror 6, sans framework de routage ni de state : routage par hash
(`router.ts`), appels HTTP et types dans `api.ts`, une vue par espace dans `components/`,
styles dans un seul `styles.css`. Le chat diffuse en SSE ; ses conversations sont gardées
dans le navigateur (`chats.ts`).

## Tests

- Les fixtures (`conftest.py`) copient `vault-exemple/` dans un dossier temporaire : ne
  jamais écrire dans `vault-exemple/` depuis un test. Modifier `vault-exemple/` peut casser
  des tests existants.
- Les appels LLM sont simulés avec `FakeLLM([réponse1, réponse2, ...])` ; aucun test ne doit
  appeler une vraie API.
- Ajouter des tests pour chaque nouvelle route ou service.

## Conventions

- Tout est en **français** : code commenté, docstrings, messages d'erreur, UI, messages de
  commit (impératif : « Ajoute … », titre court puis corps explicatif).
- Identifiants métier en français sans accents (`a_faire`, `en_cours`, `terminee`, `haute`,
  dossiers `taches/`, `reunions/`, `comptes/`…). Statuts et priorités : `STATUSES` /
  `PRIORITIES` dans `tasks.py`.
- Une nouvelle fonctionnalité met à jour le README (section Utilisation) et la roadmap /
  l'architecture de CONCEPTION.md.
- `.env` contient des secrets : ne jamais le lire ni le committer (modèle : `.env.example`).
