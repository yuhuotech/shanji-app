use crate::config;
use crate::error::{AppError, Result};
use crate::paths::AppPaths;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const DEFAULT_REGISTRY_URL: &str =
    "https://github.com/yuhuotech/shanji/releases/download/models/model_registry.json";
const DEV_REGISTRY_URL: &str = "http://localhost:1420/model_registry.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub language: String,
    pub description: String,
    pub size_bytes: u64,
    pub download_url: String,
    #[serde(default)]
    pub modelscope_url: Option<String>,
    #[serde(default)]
    pub huggingface_url: Option<String>,
    pub sha256: String,
    pub version: String,
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub is_downloaded: bool,
    #[serde(skip)]
    pub download_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRegistry {
    pub version: u32,
    pub updated_at: String,
    pub models: Vec<ModelInfo>,
}

pub fn default_registry() -> ModelRegistry {
    serde_json::from_str(include_str!("../../../public/model_registry.json")).unwrap_or_else(
        |error| {
            log::error!("Failed to parse bundled model registry: {}", error);
            ModelRegistry {
                version: 1,
                updated_at: "2026-03-12".to_string(),
                models: Vec::new(),
            }
        },
    )
}

pub fn registry_url() -> String {
    if let Ok(url) = std::env::var("SHANJI_MODEL_REGISTRY_URL") {
        return url;
    }

    if cfg!(debug_assertions) {
        DEV_REGISTRY_URL.to_string()
    } else {
        DEFAULT_REGISTRY_URL.to_string()
    }
}

pub fn list_models_with_paths(paths: &AppPaths) -> Result<Vec<ModelInfo>> {
    let mut registry = default_registry();
    apply_download_status_with_paths(paths, &mut registry);
    Ok(registry.models)
}

pub async fn fetch_registry_with_paths(paths: &AppPaths) -> Result<Vec<ModelInfo>> {
    let url = registry_url();
    let mut registry = match fetch_remote_registry(&url).await {
        Ok(registry) => {
            log::info!("Loaded model registry from {}", url);
            registry
        }
        Err(error) => {
            log::warn!(
                "Failed to load model registry from {}, falling back to bundled registry: {}",
                url,
                error
            );
            default_registry()
        }
    };

    if registry.models.is_empty() {
        log::warn!("Model registry is empty");
    }

    apply_download_status_with_paths(paths, &mut registry);
    Ok(registry.models)
}

pub async fn list_downloaded_with_paths(paths: &AppPaths) -> Result<Vec<ModelInfo>> {
    let all_models = fetch_registry_with_paths(paths).await?;
    Ok(all_models.into_iter().filter(|model| model.is_downloaded).collect())
}

pub fn delete_model_with_paths(paths: &AppPaths, model_id: &str) -> Result<bool> {
    let model_dir = get_model_dir_with_paths(paths, model_id);
    if !model_dir.exists() {
        return Ok(false);
    }

    std::fs::remove_dir_all(&model_dir)?;
    Ok(true)
}

pub fn switch_model_with_paths(paths: &AppPaths, model_id: &str) -> Result<PathBuf> {
    let model_dir = get_model_dir_with_paths(paths, model_id);
    validate_model_dir(&model_dir, &[])?;

    let mut cfg = config::get_config(paths)?;
    cfg.asr.model_id = model_id.to_string();
    config::save_config(paths, &cfg)?;

    Ok(model_dir)
}

pub fn get_model_dir_with_paths(paths: &AppPaths, model_id: &str) -> PathBuf {
    paths.models_dir().join(model_id)
}

pub fn is_model_downloaded_with_paths(paths: &AppPaths, model_id: &str) -> bool {
    let model_dir = get_model_dir_with_paths(paths, model_id);
    is_model_ready(&model_dir, &[])
}

pub fn import_local_model_with_paths(
    paths: &AppPaths,
    source_dir: &Path,
    model_name: &str,
) -> Result<String> {
    let encoder_path = source_dir.join("encoder.onnx");
    if !encoder_path.exists() {
        return Err(AppError::Io(
            "Source directory does not contain encoder.onnx".to_string(),
        ));
    }

    let model_id = format!("local-{}", model_name.to_lowercase().replace(' ', "-"));
    let model_dir = get_model_dir_with_paths(paths, &model_id);

    std::fs::create_dir_all(&model_dir)?;

    for entry in walkdir::WalkDir::new(source_dir) {
        let entry = entry.map_err(|e| AppError::Io(e.to_string()))?;
        let path = entry.path();

        if path.is_file() {
            let relative = path.strip_prefix(source_dir).unwrap_or(path);
            let dest = model_dir.join(relative);

            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }

            std::fs::copy(path, dest)?;
        }
    }

    Ok(model_id)
}

pub fn validate_model_dir(model_dir: &Path, files: &[String]) -> Result<()> {
    let missing: Vec<String> = required_files(files)
        .into_iter()
        .filter(|file| {
            let path = model_dir.join(file);
            if path.exists() {
                return false;
            }
            if file == "tokens.txt" {
                return !model_dir.join("vocab.txt").exists();
            }
            true
        })
        .collect();

    if missing.is_empty() {
        return Ok(());
    }

    Err(AppError::Model(format!(
        "Model package is incomplete, missing files: {}",
        missing.join(", ")
    )))
}

async fn fetch_remote_registry(url: &str) -> Result<ModelRegistry> {
    let client = reqwest::Client::new();
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| AppError::Network(format!("Failed to fetch model registry: {}", e)))?;

    let status = response.status();
    if !status.is_success() {
        return Err(AppError::Network(format!(
            "Model registry request failed: {}",
            status
        )));
    }

    let body = response
        .text()
        .await
        .map_err(|e| AppError::Network(format!("Failed to read model registry: {}", e)))?;

    serde_json::from_str(&body)
        .map_err(|e| AppError::Config(format!("Failed to parse model registry: {}", e)))
}

fn apply_download_status_with_paths(paths: &AppPaths, registry: &mut ModelRegistry) {
    let models_dir = paths.models_dir();

    for model in &mut registry.models {
        let model_dir = models_dir.join(&model.id);
        model.is_downloaded = is_model_ready(&model_dir, &model.files);
        model.download_path = if model.is_downloaded {
            Some(model_dir)
        } else {
            None
        };
    }
}

fn required_files(files: &[String]) -> Vec<String> {
    if files.is_empty() {
        return vec!["model.onnx".to_string(), "vocab.txt".to_string()];
    }

    files.to_vec()
}

fn has_model_file(model_dir: &Path) -> bool {
    model_dir.join("model.onnx").exists()
        || model_dir.join("model_quant.onnx").exists()
        || model_dir.join("encoder.onnx").exists()
}

fn has_vocab_file(model_dir: &Path) -> bool {
    model_dir.join("vocab.txt").exists() || model_dir.join("vocab.json").exists()
}

fn is_model_ready(model_dir: &Path, files: &[String]) -> bool {
    if !model_dir.exists() {
        return false;
    }

    if files.is_empty() {
        return has_model_file(model_dir) && has_vocab_file(model_dir);
    }

    required_files(files).iter().all(|file| {
        if model_dir.join(file).exists() {
            return true;
        }

        if file == "tokens.txt" {
            return model_dir.join("vocab.txt").exists();
        }

        false
    })
}
