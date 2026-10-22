//! Assemblage des services autour d'un vault.

use std::sync::Arc;
use std::time::Duration;

use crate::assist::AssistService;
use crate::chat::{ChatDeps, ChatService};
use crate::config::Settings;
use crate::drafts::DraftService;
use crate::error::Result;
use crate::imputations::ImputationService;
use crate::inbox::InboxService;
use crate::index::Index;
use crate::journal::JournalService;
use crate::embed_local::{local_embedder, LocalModel, MODEL_NAME};
use crate::library::LibraryService;
use crate::library_vectors::VectorConfig;
use crate::llm::Llm;
use crate::reports::ReportService;
use crate::semantic::{openai_embedder, Embedder, SemanticIndex};
use crate::tasks::TaskService;
use crate::vault::Vault;

pub struct Services {
    pub settings: Arc<Settings>,
    pub vault: Arc<Vault>,
    pub index: Arc<Index>,
    pub semantic: Arc<SemanticIndex>,
    pub tasks: Arc<TaskService>,
    pub journal: Arc<JournalService>,
    pub inbox: Arc<InboxService>,
    pub reports: Arc<ReportService>,
    pub assist: AssistService,
    pub chat: ChatService,
    pub imputations: Arc<ImputationService>,
    pub drafts: Arc<DraftService>,
    pub library: Arc<LibraryService>,
}

impl Services {
    /// Services sur le vault des réglages ; sans `embedder`, celui de l'API configurée (s'il y en a une).
    pub fn new(settings: Settings, llm: Arc<dyn Llm>, embedder: Option<Embedder>) -> Result<Arc<Self>> {
        let settings = Arc::new(settings);
        // la bibliothèque rangée dans le vault n'est ni indexée ni surveillée comme des notes
        std::fs::create_dir_all(&settings.library)?;
        let vault = Arc::new(Vault::with_excluded(&settings.vault, std::slice::from_ref(&settings.library))?);
        let tempo_dir = vault.root.join(".tempo");
        let index = Arc::new(Index::new(vault.clone(), &tempo_dir.join("index.db"))?);
        // embeddings : ceux fournis (tests), sinon l'API configurée, sinon le modèle local livré
        // avec l'application ; le seuil de similarité dépend du modèle (e5 note tout haut)
        let embedding: Option<(Embedder, String, f32)> = match embedder {
            Some(e) => Some((e, settings.embed_model.clone().unwrap_or_else(|| "test".into()), 0.05)),
            None if settings.embed_enabled() => {
                Some((openai_embedder(&settings)?, settings.embed_model.clone().unwrap_or_default(), 0.5))
            }
            None if settings.embed_local => LocalModel::discover(settings.embed_local_dir.as_deref(), settings.onnxruntime.as_deref())
                .map(|model| (local_embedder(model), format!("local:{MODEL_NAME}"), 0.83)),
            None => None,
        };
        let semantic = Arc::new(SemanticIndex::new(
            vault.clone(),
            index.clone(),
            embedding.as_ref().map(|(e, _, _)| e.clone()),
            &tempo_dir.join("vectors.db"),
            embedding.as_ref().map(|(_, model, _)| model.as_str()),
        )?);
        let vectors = embedding.map(|(embedder, model, min_similarity)| VectorConfig {
            embedder,
            model,
            min_similarity: settings.embed_min_similarity.unwrap_or(min_similarity),
        });
        let tasks = Arc::new(TaskService::new(vault.clone(), index.clone()));
        let journal = Arc::new(JournalService::new(vault.clone(), index.clone()));
        let reports = Arc::new(ReportService::new(vault.clone(), index.clone(), tasks.clone(), llm.clone(), settings.clone()));
        let inbox = Arc::new(InboxService::new(vault.clone(), index.clone(), tasks.clone(), journal.clone(), llm.clone(), settings.clone()));
        let drafts = Arc::new(DraftService::new(vault.clone(), index.clone(), tasks.clone(), journal.clone(), llm.clone(), settings.clone()));
        let imputations = Arc::new(ImputationService::new(vault.clone(), index.clone(), reports.clone(), llm.clone(), settings.clone()));
        let library = Arc::new(LibraryService::new(settings.clone(), vault.clone(), index.clone(), llm.clone(), vectors)?);
        let deps = ChatDeps {
            vault: vault.clone(),
            index: index.clone(),
            tasks: tasks.clone(),
            journal: journal.clone(),
            inbox: inbox.clone(),
            reports: reports.clone(),
            imputations: imputations.clone(),
            drafts: drafts.clone(),
            library: Some(library.clone()),
        };
        Ok(Arc::new(Services {
            assist: AssistService::new(vault.clone(), index.clone(), tasks.clone(), llm.clone(), settings.clone()),
            chat: ChatService::new(deps, llm, settings.clone(), Some(semantic.clone())),
            inbox,
            drafts,
            imputations,
            library,
            settings,
            vault,
            index,
            semantic,
            tasks,
            journal,
            reports,
        }))
    }

    /// Indexe le vault puis, si demandé, surveille le dossier et vectorise en arrière-plan.
    pub fn start(&self) -> Result<usize> {
        let count = self.index.rebuild()?;
        if self.settings.watch {
            self.index.start_watching()?;
            self.semantic.start(Duration::from_secs(15));
        }
        self.library.start(self.settings.watch)?;
        Ok(count)
    }

    pub fn stop(&self) {
        self.semantic.stop();
        self.library.stop();
        self.index.stop_watching();
    }
}
