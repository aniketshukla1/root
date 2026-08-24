//! Error type shared by every backend module.
//!
//! `AppError` serializes to a plain string so it can cross the Tauri command
//! boundary and surface directly in the UI.

use serde::{Serialize, Serializer};

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("configuration error: {0}")]
    Config(String),

    #[error("no connection named '{0}' is configured")]
    UnknownProfile(String),

    #[error("{service} is unreachable: {detail}")]
    Unreachable { service: String, detail: String },

    #[error("collector error: {0}")]
    Collector(String),

    #[error("assistant error: {0}")]
    Llm(String),

    #[error("credential store: {0}")]
    Secret(String),

    #[error("this action is not supported by {0}")]
    UnsupportedAction(String),

    #[error("{0}")]
    Rejected(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, AppError>;

impl Serialize for AppError {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl From<reqwest::Error> for AppError {
    fn from(err: reqwest::Error) -> Self {
        AppError::Collector(err.to_string())
    }
}

impl From<sqlx::Error> for AppError {
    fn from(err: sqlx::Error) -> Self {
        AppError::Collector(err.to_string())
    }
}

impl From<bollard::errors::Error> for AppError {
    fn from(err: bollard::errors::Error) -> Self {
        AppError::Collector(err.to_string())
    }
}
