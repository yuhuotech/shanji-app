use serde::Serialize;
use thiserror::Error;

pub type Result<T> = std::result::Result<T, AppError>;

#[derive(Error, Debug, Serialize, Clone)]
#[serde(tag = "type", content = "message")]
pub enum AppError {
    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Audio error: {0}")]
    Audio(String),

    #[error("ASR engine error: {0}")]
    Asr(String),

    #[error("Model error: {0}")]
    Model(String),

    #[error("Network error: {0}")]
    Network(String),

    #[error("IO error: {0}")]
    Io(String),

    #[error("Permission denied: {0}")]
    Permission(String),

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Invalid input: {0}")]
    InvalidInput(String),

    #[error("Output error: {0}")]
    Output(String),

    #[error("Internal error: {0}")]
    Internal(String),
}

impl From<std::io::Error> for AppError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err.to_string())
    }
}

impl From<serde_json::Error> for AppError {
    fn from(err: serde_json::Error) -> Self {
        Self::Config(err.to_string())
    }
}

impl From<anyhow::Error> for AppError {
    fn from(err: anyhow::Error) -> Self {
        Self::Internal(err.to_string())
    }
}

pub fn to_app_error<T, E: std::fmt::Display>(
    result: std::result::Result<T, E>,
    category: fn(String) -> AppError,
) -> Result<T> {
    result.map_err(|e| category(e.to_string()))
}
