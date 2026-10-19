//! Erreurs métier et leur traduction en réponses HTTP.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Requête invalide (chemin hors du vault, valeur incorrecte…) : 400.
    #[error("{0}")]
    Vault(String),
    /// Note, tâche ou compte introuvable : 404.
    #[error("{0}")]
    NotFound(String),
    /// Le fichier a été modifié ailleurs depuis la dernière lecture : 409.
    #[error("Le fichier a été modifié par ailleurs")]
    Conflict { current_content: String },
    /// API LLM non configurée : 503.
    #[error("API LLM non configurée : renseignez VIBE_BASE_URL et VIBE_API_KEY dans .env")]
    LlmNotConfigured,
    /// Erreur de l'API LLM ou réponse inexploitable : 502.
    #[error("{0}")]
    Llm(String),
    /// Erreur inattendue (base SQLite, disque…) : 500.
    #[error("{0}")]
    Internal(String),
}

pub type Result<T> = std::result::Result<T, Error>;

pub fn vault_err(message: impl Into<String>) -> Error {
    Error::Vault(message.into())
}

pub fn not_found(message: impl Into<String>) -> Error {
    Error::NotFound(message.into())
}

impl Error {
    pub fn status(&self) -> StatusCode {
        match self {
            Error::Vault(_) => StatusCode::BAD_REQUEST,
            Error::NotFound(_) => StatusCode::NOT_FOUND,
            Error::Conflict { .. } => StatusCode::CONFLICT,
            Error::LlmNotConfigured => StatusCode::SERVICE_UNAVAILABLE,
            Error::Llm(_) => StatusCode::BAD_GATEWAY,
            Error::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// Erreur d'appel au modèle (LLM ou embeddings), y compris l'absence de configuration.
    pub fn is_llm(&self) -> bool {
        matches!(self, Error::Llm(_) | Error::LlmNotConfigured)
    }
}

impl From<rusqlite::Error> for Error {
    fn from(err: rusqlite::Error) -> Self {
        Error::Internal(format!("Erreur de l'index SQLite : {err}"))
    }
}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Error::Internal(format!("Erreur d'accès au disque : {err}"))
    }
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let body = match &self {
            Error::Conflict { current_content } => {
                json!({"detail": self.to_string(), "current_content": current_content})
            }
            _ => json!({"detail": self.to_string()}),
        };
        (self.status(), Json(body)).into_response()
    }
}
