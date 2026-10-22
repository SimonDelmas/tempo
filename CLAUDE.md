# CLAUDE.md

Tempo : client lourd en Rust (Tauri 2 + React) pour notes Markdown, tâches, inbox mails/Teams,
rapports et imputations, assisté par l'API LLM interne Vibe (compatible OpenAI).
Fonctionnalités : [README.md](README.md). Architecture, formats du vault et roadmap :
[CONCEPTION.md](CONCEPTION.md) — à lire avant toute évolution, et à tenir à jour.

## Commandes

Python n'est pas disponible dans ce repo.

```bash
cargo test                               # tests du cœur (crates/tempo-core)
cargo test -p tempo-core --test chat nom # un test d'un fichier de tests/
cargo run -p tempo-core --bin tempo-server   # serveur seul sur http://127.0.0.1:8765 (sert frontend/dist)
cargo run -p tempo-app                   # application de bureau (build de développement)
cd src-tauri && cargo tauri build        # installeurs NSIS / MSI dans target/release/bundle/
cd frontend && npm run build             # compile l'interface (tsc -b + vite build)
cd frontend && npm run typecheck         # vérification TypeScript seule
cd frontend && npm run dev               # Vite sur :5173, proxy /api → :8765 (lancer tempo-server à côté)
```

Plateforme de dev : Windows, toolchain Rust `msvc`. L'application de bureau embarque
`frontend/dist` à la compilation : recompiler l'interface avant `cargo run -p tempo-app`.
Pas de linter configuré (`cargo clippy` disponible).

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

## Cœur (`crates/tempo-core/src/`)

- `api.rs` : `router(services, ApiOptions)` — toutes les routes `/api/*` (axum), corps des
  requêtes, SSE du chat, service du frontend (dossier ou assets embarqués) et jeton d'accès
  optionnel. Les traitements sont synchrones et passent par `blocking(...)`
  (`spawn_blocking`). Un service par domaine (`tasks.rs`, `inbox.rs`, `reports.rs`,
  `imputations.rs`, `assist.rs`, `chat.rs`, `semantic.rs`…), assemblés dans `services.rs`.
- `vault.rs` : lecture/écriture avec frontmatter YAML (`Meta` = `serde_json::Map` ordonnée,
  émission au style PyYAML : dates sans guillemets, listes simples en ligne) et détection de
  conflit par hash (`expected_hash`) — passer par lui pour toute écriture ; retirer une clé
  avec `shift_remove` pour garder l'ordre.
- `index.rs` : index SQLite (notes, liens, FTS5) + watcher `notify`.
- `llm.rs` : trait `Llm`, client OpenAI-compatible (`ureq`, certificats Windows), mode JSON
  (`chat_json`, `extract_json`, `ensure_json_hint` : le mot « json » doit figurer dans un
  message utilisateur, pas seulement système), tool calling natif diffusé (`chat_tools`,
  utilisé par le chat), et `FakeLlm` pour les tests (`fake_tool_calls` simule des appels
  d'outils).
- `library.rs` : bibliothèque de documents (pivots `<nom>.md` + `sources/`, dossier `TEMPO_LIBRARY`
  exclu de l'index des notes, index `library.db` rangé hors du vault, file « À classer », thèmes
  `themes/`) ; `convert/` : conversion en Markdown (pandoc, pdfium, pptx, calamine, mails, .doc/.ppt).
  Les tests de conversion pandoc / PDF sont sautés si les outils sont absents.
  `library_vectors.rs` : vecteurs des passages et recherche hybride (RRF) ; `embed_local.rs` :
  embeddings locaux (multilingual-e5-small, ONNX Runtime via `ort` figé en 2.0.0-rc.13 — la rc.15
  exige Rust 1.92 —, modèle chargé à la demande). Le test du modèle local est sauté s'il est absent.
  `library_agent.rs` : outils de l'agent sur la bibliothèque (`search_documents`, `read_document`,
  `list_documents`) et vérification des citations `[[document#section]]` / `[[document#p. N]]`.
- `notes.rs` : modifications ciblées d'une note (passages, sections, frontmatter, liens),
  utilisées par les propositions du chat.
- `de.rs` : désérialiseurs tolérants pour les réponses du modèle (équivalent des validateurs
  Pydantic : valeur invalide → valeur par défaut).
- `prompts.rs` : prompts centralisés.
- `error.rs` : `Error` (Vault 400, NotFound 404, Conflict 409, LlmNotConfigured 503, Llm 502).
- `config.rs` : `Settings` chargé depuis l'environnement et les `.env` (`TEMPO_VAULT`,
  `VIBE_*`, `TEMPO_MODEL_FAST`, `TEMPO_MODEL_SMART`, `TEMPO_EMBED_*`). Sans `VIBE_*`, les
  fonctions IA renvoient `LlmNotConfigured` mais le reste fonctionne ; sans
  `TEMPO_EMBED_MODEL`, la recherche sémantique utilise le modèle local s'il est présent
  (`Settings::for_vault` le désactive : les tests passent `hashing_embedder` à `TestApp`).

## Application de bureau (`src-tauri/`)

`main.rs` : charge les réglages, démarre les services, sert le routeur sur
`127.0.0.1:48765` (`TEMPO_DESKTOP_PORT`, port libre en repli) avec un jeton (cookie HttpOnly) et les assets embarqués par
Tauri, puis ouvre la fenêtre. Instance unique, liens externes ouverts dans le navigateur.

## Frontend (`frontend/src/`)

React 19 + Vite + CodeMirror 6, sans framework de routage ni de state : routage par hash
(`router.ts`), appels HTTP relatifs (`/api/*`) et types dans `api.ts`, une vue par espace
dans `components/`, styles dans un seul `styles.css`. Le chat diffuse en SSE ; ses
conversations sont gardées dans le stockage local (`chats.ts`).

## Tests

- Tests unitaires dans chaque module (`#[cfg(test)]`), tests d'intégration dans
  `crates/tempo-core/tests/` ; `tests/common/mod.rs` copie `vault-exemple/` dans un dossier
  temporaire (`ctx()`, `TestApp` pour l'API) : ne jamais écrire dans `vault-exemple/` depuis
  un test. Modifier `vault-exemple/` peut casser des tests existants.
- Les appels LLM sont simulés avec `FakeLlm::new([réponse1, réponse2, ...])` (appels
  consultables avec `.calls()`) ; aucun test ne doit appeler une vraie API (le client HTTP est
  testé contre un serveur local, `tests/llm.rs`).
- Ajouter des tests pour chaque nouvelle route ou service.

## Conventions

- Tout est en **français** : code commenté, doc-comments, messages d'erreur, UI, messages de
  commit (impératif : « Ajoute … », titre court puis corps explicatif).
- Identifiants métier en français sans accents (`a_faire`, `en_cours`, `terminee`, `haute`,
  dossiers `taches/`, `reunions/`, `comptes/`…). Statuts et priorités : `STATUSES` /
  `PRIORITIES` dans `tasks.rs`.
- Les réponses JSON de l'API gardent les noms de champs attendus par `frontend/src/api.ts`.
- Une nouvelle fonctionnalité met à jour le README (section Utilisation) et la roadmap /
  l'architecture de CONCEPTION.md.
- `.env` contient des secrets : ne jamais le lire ni le committer (modèle : `.env.example`).
