//! Tempo : notes Markdown, tâches, inbox, rapports et imputations assistés par LLM.
//!
//! Le vault Markdown est la source de vérité ; les bases SQLite (`<vault>/.tempo/`) ne sont
//! que des caches reconstructibles. Ce crate fournit les services et l'API HTTP (`/api/*`),
//! utilisés par le serveur autonome (`tempo-server`) et par l'application de bureau (Tauri).

pub mod api;
pub mod assist;
pub mod chat;
pub mod config;
pub mod convert;
pub mod de;
pub mod drafts;
pub mod error;
pub mod imputations;
pub mod inbox;
pub mod index;
pub mod journal;
pub mod library;
pub mod library_agent;
pub mod llm;
pub mod notes;
pub mod prompts;
pub mod reports;
pub mod semantic;
pub mod services;
pub mod tasks;
pub mod util;
pub mod vault;

pub use api::{router, ApiOptions, AssetFn, Frontend};
pub use config::{load_settings, Settings};
pub use error::{Error, Result};
pub use services::Services;
