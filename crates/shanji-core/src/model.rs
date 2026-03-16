use crate::config;
use crate::error::{AppError, Result};
use crate::paths::AppPaths;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const DEFAULT_REGISTRY_URL: &str =
    "https://github.com/yuhuotech/paraformer-zh/releases/download/models/model_registry.json";
const DEV_REGISTRY_URL: &str = "http://localhost:1420/model_registry.json";
const GITHUB_PROXY_ENV: &str = "SHANJI_GITHUB_PROXY";
const DEFAULT_GITHUB_PROXY: &str = "https://ghfast.top/";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ModelBackend {
    Whole,
    Streaming,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ModelArtifactRole {
    Model,
    ModelQuant,
    Encoder,
    EncoderQuant,
    Decoder,
    DecoderQuant,
    Config,
    Vocab,
    MeanVariance,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelArtifact {
    pub role: ModelArtifactRole,
    pub file_name: String,
    #[serde(default)]
    pub size_bytes: u64,
    #[serde(default)]
    pub sha256: String,
}

#[derive(Debug, Clone)]
pub struct ResolvedModelLayout {
    pub model_dir: PathBuf,
    pub backend: ModelBackend,
    pub model_path: Option<PathBuf>,
    pub model_quant_path: Option<PathBuf>,
    pub encoder_path: Option<PathBuf>,
    pub encoder_quant_path: Option<PathBuf>,
    pub decoder_path: Option<PathBuf>,
    pub decoder_quant_path: Option<PathBuf>,
    pub vocab_path: PathBuf,
    pub config_path: Option<PathBuf>,
    pub mean_variance_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub language: String,
    pub description: String,
    pub backend: ModelBackend,
    pub size_bytes: u64,
    pub download_url: String,
    pub sha256: String,
    pub version: String,
    pub artifacts: Vec<ModelArtifact>,
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

impl ModelInfo {
    pub fn artifact(&self, role: ModelArtifactRole) -> Option<&ModelArtifact> {
        self.artifacts.iter().find(|artifact| artifact.role == role)
    }
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
    if cfg!(debug_assertions) {
        DEV_REGISTRY_URL.to_string()
    } else {
        DEFAULT_REGISTRY_URL.to_string()
    }
}

fn github_proxy_prefix_with_paths(paths: Option<&AppPaths>) -> Option<String> {
    if let Some(paths) = paths {
        if let Ok(cfg) = config::get_config(paths) {
            let value = cfg.network.github_proxy.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }

    if let Ok(raw) = std::env::var(GITHUB_PROXY_ENV) {
        let value = raw.trim();
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }

    Some(DEFAULT_GITHUB_PROXY.to_string())
}

fn apply_github_proxy_with_prefix(url: &str, prefix: Option<&str>) -> String {
    let Some(prefix) = prefix else {
        return url.to_string();
    };

    if !is_github_asset_url(url) || url.starts_with(prefix) {
        return url.to_string();
    }

    if prefix.contains("{url}") {
        return prefix.replace("{url}", url);
    }

    let separator = if prefix.ends_with('/') { "" } else { "/" };
    format!("{prefix}{separator}{url}")
}

fn is_github_asset_url(url: &str) -> bool {
    url.contains("://github.com/") || url.contains("://raw.githubusercontent.com/")
}

pub fn list_models_with_paths(paths: &AppPaths) -> Result<Vec<ModelInfo>> {
    let mut registry = default_registry();
    apply_download_status_with_paths(paths, &mut registry);
    Ok(registry.models)
}

pub async fn fetch_registry_with_paths(paths: &AppPaths) -> Result<Vec<ModelInfo>> {
    let direct_url = if let Ok(url) = std::env::var("SHANJI_MODEL_REGISTRY_URL") {
        url
    } else {
        registry_url()
    };
    let proxy_prefix = github_proxy_prefix_with_paths(Some(paths));
    let proxied_url = apply_github_proxy_with_prefix(&direct_url, proxy_prefix.as_deref());
    let mut registry = match fetch_remote_registry(&proxied_url).await {
        Ok(registry) => {
            log::info!("Loaded model registry from {}", proxied_url);
            registry
        }
        Err(error) => {
            if proxied_url != direct_url {
                log::warn!(
                    "Failed to load model registry from proxy {}, retrying direct: {}",
                    proxied_url,
                    error
                );
                match fetch_remote_registry(&direct_url).await {
                    Ok(registry) => {
                        log::info!("Loaded model registry from {}", direct_url);
                        registry
                    }
                    Err(direct_error) => {
                        log::warn!(
                            "Failed to load model registry from {}, falling back to bundled registry: {}",
                            direct_url,
                            direct_error
                        );
                        default_registry()
                    }
                }
            } else {
                log::warn!(
                    "Failed to load model registry from {}, falling back to bundled registry: {}",
                    proxied_url,
                    error
                );
                default_registry()
            }
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
    Ok(all_models
        .into_iter()
        .filter(|model| model.is_downloaded)
        .collect())
}

pub fn get_model_info(model_id: &str) -> Result<ModelInfo> {
    default_registry()
        .models
        .into_iter()
        .find(|model| model.id == model_id)
        .ok_or_else(|| AppError::Model(format!("Model '{}' not found in registry", model_id)))
}

pub fn resolve_model_layout_with_paths(
    paths: &AppPaths,
    model_id: &str,
) -> Result<ResolvedModelLayout> {
    let model = get_model_info(model_id)?;
    let model_dir = get_model_dir_with_paths(paths, model_id);
    validate_model_dir(&model_dir, &model)?;
    build_model_layout(&model_dir, &model)
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
    let model = get_model_info(model_id)?;
    let model_dir = get_model_dir_with_paths(paths, model_id);
    validate_model_dir(&model_dir, &model)?;

    let mut cfg = config::get_config(paths)?;
    cfg.asr.live_model_id = model_id.to_string();
    config::save_config(paths, &cfg)?;

    Ok(model_dir)
}

pub fn get_model_dir_with_paths(paths: &AppPaths, model_id: &str) -> PathBuf {
    paths.models_dir().join(model_id)
}

pub fn is_model_downloaded_with_paths(paths: &AppPaths, model_id: &str) -> bool {
    let Ok(model) = get_model_info(model_id) else {
        return false;
    };
    let model_dir = get_model_dir_with_paths(paths, model_id);
    is_model_ready(&model_dir, &model)
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

pub fn validate_model_dir(model_dir: &Path, model: &ModelInfo) -> Result<()> {
    validate_model_metadata(model)?;

    let missing: Vec<String> = model
        .artifacts
        .iter()
        .filter_map(|artifact| {
            if model_dir.join(&artifact.file_name).exists() {
                None
            } else {
                Some(artifact.file_name.clone())
            }
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

fn validate_model_metadata(model: &ModelInfo) -> Result<()> {
    let has_role = |role| model.artifact(role).is_some();

    match model.backend {
        ModelBackend::Whole => {
            for role in [
                ModelArtifactRole::Config,
                ModelArtifactRole::Vocab,
                ModelArtifactRole::MeanVariance,
            ] {
                if !has_role(role) {
                    return Err(AppError::Model(format!(
                        "Model '{}' must define a {:?} artifact",
                        model.id, role
                    )));
                }
            }
            if !has_role(ModelArtifactRole::Model) && !has_role(ModelArtifactRole::ModelQuant) {
                return Err(AppError::Model(format!(
                    "Model '{}' must define a model or modelQuant artifact",
                    model.id
                )));
            }
        }
        ModelBackend::Streaming => {
            for role in [
                ModelArtifactRole::Encoder,
                ModelArtifactRole::Decoder,
                ModelArtifactRole::Config,
                ModelArtifactRole::Vocab,
                ModelArtifactRole::MeanVariance,
            ] {
                if !has_role(role) {
                    return Err(AppError::Model(format!(
                        "Model '{}' must define a {:?} artifact",
                        model.id, role
                    )));
                }
            }
        }
    }

    Ok(())
}

fn build_model_layout(model_dir: &Path, model: &ModelInfo) -> Result<ResolvedModelLayout> {
    let artifact_path = |role| {
        model
            .artifact(role)
            .map(|artifact| model_dir.join(&artifact.file_name))
    };

    Ok(ResolvedModelLayout {
        model_dir: model_dir.to_path_buf(),
        backend: model.backend,
        model_path: artifact_path(ModelArtifactRole::Model),
        model_quant_path: artifact_path(ModelArtifactRole::ModelQuant),
        encoder_path: artifact_path(ModelArtifactRole::Encoder),
        encoder_quant_path: artifact_path(ModelArtifactRole::EncoderQuant),
        decoder_path: artifact_path(ModelArtifactRole::Decoder),
        decoder_quant_path: artifact_path(ModelArtifactRole::DecoderQuant),
        vocab_path: artifact_path(ModelArtifactRole::Vocab).ok_or_else(|| {
            AppError::Model(format!("Model '{}' is missing a vocab artifact", model.id))
        })?,
        config_path: artifact_path(ModelArtifactRole::Config),
        mean_variance_path: artifact_path(ModelArtifactRole::MeanVariance),
    })
}

/// Download and install the model, calling `progress_fn(downloaded_bytes, total_bytes)`.
/// Downloads individual files from the configured GitHub release base URL.
/// Blocks the calling thread; run from a background thread.
pub fn download_model_with_progress<F>(
    paths: &AppPaths,
    model_id: &str,
    progress_fn: F,
) -> Result<()>
where
    F: Fn(u64, u64) + Send,
{
    let registry = default_registry();
    let model = registry
        .models
        .iter()
        .find(|m| m.id == model_id)
        .ok_or_else(|| AppError::Model(format!("Model '{}' not found in registry", model_id)))?;
    validate_model_metadata(model)?;
    if model.download_url.is_empty() {
        return Err(AppError::Model(format!(
            "No download URL configured for model '{}'",
            model_id
        )));
    }

    let client = build_blocking_client()?;
    let models_dir = paths.models_dir();
    std::fs::create_dir_all(&models_dir)?;
    let model_dir = get_model_dir_with_paths(paths, model_id);
    std::fs::create_dir_all(&model_dir)?;

    let proxy_prefix = github_proxy_prefix_with_paths(Some(paths));
    download_individual_files(
        &client,
        &model.download_url,
        proxy_prefix.as_deref(),
        &model_dir,
        model_id,
        model.size_bytes,
        &model.artifacts,
        &progress_fn,
    )?;

    validate_model_dir(&model_dir, model)?;
    Ok(())
}

fn build_blocking_client() -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(10))
        .connect_timeout(std::time::Duration::from_secs(30))
        // No overall timeout — we handle read stalls ourselves via recv_timeout
        .build()
        .map_err(|e| AppError::Network(format!("Failed to build HTTP client: {}", e)))
}

/// Download individual model files from a GitHub release base URL.
/// Files are fetched as `{base_url}/{artifact.file_name}` and saved using the same
/// standardized file name inside the local model directory.
fn download_individual_files<F>(
    client: &reqwest::blocking::Client,
    base_url: &str,
    proxy_prefix: Option<&str>,
    model_dir: &Path,
    model_id: &str,
    total_size_hint: u64,
    artifacts: &[ModelArtifact],
    progress_fn: &F,
) -> Result<()>
where
    F: Fn(u64, u64),
{
    let base = if base_url.ends_with('/') {
        base_url.to_string()
    } else {
        format!("{}/", base_url)
    };

    let total: u64 = artifacts.iter().map(|artifact| artifact.size_bytes).sum();
    let total = total.max(total_size_hint);
    let mut downloaded_total = 0u64;

    for artifact in artifacts {
        let direct_url = format!("{}{}", base, artifact.file_name);
        let file_url = apply_github_proxy_with_prefix(&direct_url, proxy_prefix);
        let dest = model_dir.join(&artifact.file_name);

        eprintln!(
            "[shanji] Downloading {} from {}",
            artifact.file_name, file_url
        );
        match download_single_file(
            client,
            &file_url,
            &dest,
            &artifact.file_name,
            &mut downloaded_total,
            total,
            progress_fn,
        ) {
            Ok(()) => {}
            Err(proxy_error) if file_url != direct_url => {
                eprintln!(
                    "[shanji] Proxy download failed for {}, retrying direct: {}",
                    artifact.file_name, proxy_error
                );
                download_single_file(
                    client,
                    &direct_url,
                    &dest,
                    &artifact.file_name,
                    &mut downloaded_total,
                    total,
                    progress_fn,
                )?;
            }
            Err(error) => return Err(error),
        }
    }

    let _ = model_id;
    Ok(())
}

fn download_single_file<F>(
    client: &reqwest::blocking::Client,
    url: &str,
    dest: &Path,
    file_name: &str,
    downloaded_total: &mut u64,
    total: u64,
    progress_fn: &F,
) -> Result<()>
where
    F: Fn(u64, u64),
{
    use std::io::Write;
    use std::sync::mpsc;

    let response = client
        .get(url)
        .send()
        .map_err(|e| AppError::Network(format!("Request failed for {}: {}", file_name, e)))?;

    if !response.status().is_success() {
        return Err(AppError::Network(format!(
            "HTTP {} when downloading {} from {}",
            response.status(),
            file_name,
            url
        )));
    }

    let (tx, rx) = mpsc::channel::<std::result::Result<Vec<u8>, String>>();
    let mut body = response;
    std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = vec![0u8; 16_384];
        loop {
            match body.read(&mut buf) {
                Ok(0) => {
                    let _ = tx.send(Ok(vec![]));
                    break;
                }
                Ok(n) => {
                    if tx.send(Ok(buf[..n].to_vec())).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(e.to_string()));
                    break;
                }
            }
        }
    });

    let mut file = std::fs::File::create(dest)
        .map_err(|e| AppError::Io(format!("Cannot create {}: {}", file_name, e)))?;
    let timeout = std::time::Duration::from_secs(60);
    loop {
        match rx.recv_timeout(timeout) {
            Ok(Ok(chunk)) if chunk.is_empty() => break,
            Ok(Ok(chunk)) => {
                file.write_all(&chunk)
                    .map_err(|e| AppError::Io(format!("Write error for {}: {}", file_name, e)))?;
                *downloaded_total += chunk.len() as u64;
                progress_fn((*downloaded_total).min(total), total);
            }
            Ok(Err(e)) => {
                return Err(AppError::Io(format!("Read error for {}: {}", file_name, e)));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                return Err(AppError::Network(format!(
                    "Stalled while downloading {}: no data for 60 seconds",
                    file_name
                )));
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    Ok(())
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
        model.is_downloaded = is_model_ready(&model_dir, model);
        model.download_path = if model.is_downloaded {
            Some(model_dir)
        } else {
            None
        };
    }
}

fn is_model_ready(model_dir: &Path, model: &ModelInfo) -> bool {
    if !model_dir.exists() || validate_model_metadata(model).is_err() {
        return false;
    }

    model
        .artifacts
        .iter()
        .all(|artifact| model_dir.join(&artifact.file_name).exists())
}
