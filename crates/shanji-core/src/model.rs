use crate::config;
use crate::error::{AppError, Result};
use crate::network;
use crate::paths::AppPaths;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ModelBackend {
    Whole,
    Streaming,
    Auxiliary,
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
    Metadata,
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
    pub dependencies: Vec<String>,
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

    pub fn is_auxiliary(&self) -> bool {
        self.backend == ModelBackend::Auxiliary
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

pub fn list_models_with_paths(paths: &AppPaths) -> Result<Vec<ModelInfo>> {
    let mut registry = default_registry();
    apply_download_status_with_paths(paths, &mut registry);
    Ok(registry.models)
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
    if model.is_auxiliary() {
        return Err(AppError::Model(format!(
            "Auxiliary model '{}' cannot be loaded as an ASR model",
            model_id
        )));
    }
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
    if model.is_auxiliary() {
        return Err(AppError::Model(format!(
            "Auxiliary model '{}' cannot be selected as the active ASR model",
            model_id
        )));
    }
    let model_dir = get_model_dir_with_paths(paths, model_id);
    if !is_model_downloaded_with_paths(paths, model_id) {
        return Err(AppError::Model(format!(
            "Model '{}' or its dependencies are not ready",
            model_id
        )));
    }

    let mut cfg = config::get_config(paths)?;
    cfg.asr.live_model_id = model_id.to_string();
    config::save_config(paths, &cfg)?;

    Ok(model_dir)
}

pub fn get_model_dir_with_paths(paths: &AppPaths, model_id: &str) -> PathBuf {
    paths.models_dir().join(model_id)
}

pub fn is_model_downloaded_with_paths(paths: &AppPaths, model_id: &str) -> bool {
    let registry = default_registry();
    let Some(model) = registry.models.iter().find(|model| model.id == model_id) else {
        return false;
    };
    is_model_ready_with_registry(paths.models_dir().as_path(), &registry, model)
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
        ModelBackend::Auxiliary => {
            if model.artifacts.is_empty() {
                return Err(AppError::Model(format!(
                    "Auxiliary model '{}' must define at least one artifact",
                    model.id
                )));
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
    download_models_with_progress(paths, std::slice::from_ref(&model_id), progress_fn)
}

pub fn download_model_bundle_with_progress<F>(
    paths: &AppPaths,
    model_id: &str,
    progress_fn: F,
) -> Result<()>
where
    F: Fn(u64, u64) + Send,
{
    let registry = default_registry();
    let plan = collect_dependency_plan(&registry, model_id)?;
    let ids = plan
        .iter()
        .map(|model| model.id.as_str())
        .collect::<Vec<_>>();
    download_models_with_progress(paths, &ids, progress_fn)
}

fn download_models_with_progress<F>(
    paths: &AppPaths,
    model_ids: &[&str],
    progress_fn: F,
) -> Result<()>
where
    F: Fn(u64, u64) + Send,
{
    let registry = default_registry();
    let mut plan = Vec::new();
    for model_id in model_ids {
        plan.extend(collect_dependency_plan(&registry, model_id)?);
    }
    dedupe_model_plan(&mut plan);

    let network_config = config::get_config(paths).ok().map(|cfg| cfg.network);
    let client = network::build_download_blocking_client(network_config.as_ref())?;
    let models_dir = paths.models_dir();
    std::fs::create_dir_all(&models_dir)?;

    let pending = plan
        .into_iter()
        .filter(|model| !is_model_self_ready(&models_dir.join(&model.id), model))
        .collect::<Vec<_>>();

    let total: u64 = pending
        .iter()
        .map(|model| {
            let artifact_total: u64 = model
                .artifacts
                .iter()
                .map(|artifact| artifact.size_bytes)
                .sum();
            artifact_total.max(model.size_bytes)
        })
        .sum();

    if pending.is_empty() {
        progress_fn(1, 1);
        return Ok(());
    }

    let total = total.max(1);
    let mut downloaded_total = 0u64;

    for model in pending {
        validate_model_metadata(model)?;
        if model.download_url.is_empty() {
            return Err(AppError::Model(format!(
                "No download URL configured for model '{}'",
                model.id
            )));
        }
        let model_dir = get_model_dir_with_paths(paths, &model.id);
        std::fs::create_dir_all(&model_dir)?;
        download_registered_model_files(
            &client,
            model,
            &model_dir,
            &mut downloaded_total,
            total,
            &progress_fn,
        )?;
        validate_model_dir(&model_dir, model)?;
    }

    progress_fn(total, total);
    Ok(())
}

/// Download individual model files from a GitHub release base URL.
/// Files are fetched as `{base_url}/{artifact.file_name}` and saved using the same
/// standardized file name inside the local model directory.
fn download_registered_model_files<F>(
    client: &reqwest::blocking::Client,
    model: &ModelInfo,
    model_dir: &Path,
    downloaded_total: &mut u64,
    total: u64,
    progress_fn: &F,
) -> Result<()>
where
    F: Fn(u64, u64),
{
    let base_url = &model.download_url;
    let base = if base_url.ends_with('/') {
        base_url.to_string()
    } else {
        format!("{}/", base_url)
    };

    for artifact in &model.artifacts {
        let direct_url = format!("{}{}", base, artifact.file_name);
        let dest = model_dir.join(&artifact.file_name);
        let is_complete_existing_file = dest
            .metadata()
            .map(|metadata| artifact.size_bytes == 0 || metadata.len() == artifact.size_bytes)
            .unwrap_or(false);

        if is_complete_existing_file {
            *downloaded_total = downloaded_total.saturating_add(artifact.size_bytes);
            progress_fn((*downloaded_total).min(total), total);
            continue;
        }

        eprintln!(
            "[shanji] Downloading {} from {}",
            artifact.file_name, direct_url
        );
        download_single_file(
            client,
            &direct_url,
            &dest,
            &artifact.file_name,
            downloaded_total,
            total,
            progress_fn,
        )?;
    }

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

fn apply_download_status_with_paths(paths: &AppPaths, registry: &mut ModelRegistry) {
    let models_dir = paths.models_dir();
    let statuses = registry
        .models
        .iter()
        .map(|model| {
            let model_dir = models_dir.join(&model.id);
            let is_downloaded = is_model_ready_with_registry(models_dir.as_path(), registry, model);
            let download_path = if is_downloaded { Some(model_dir) } else { None };
            (is_downloaded, download_path)
        })
        .collect::<Vec<_>>();

    for (model, (is_downloaded, download_path)) in registry.models.iter_mut().zip(statuses) {
        model.is_downloaded = is_downloaded;
        model.download_path = download_path;
    }
}

fn is_model_self_ready(model_dir: &Path, model: &ModelInfo) -> bool {
    if !model_dir.exists() || validate_model_metadata(model).is_err() {
        return false;
    }

    model
        .artifacts
        .iter()
        .all(|artifact| model_dir.join(&artifact.file_name).exists())
}

fn is_model_ready_with_registry(
    models_dir: &Path,
    registry: &ModelRegistry,
    model: &ModelInfo,
) -> bool {
    let mut visiting = std::collections::BTreeSet::new();
    is_model_ready_with_registry_inner(models_dir, registry, model, &mut visiting)
}

fn is_model_ready_with_registry_inner(
    models_dir: &Path,
    registry: &ModelRegistry,
    model: &ModelInfo,
    visiting: &mut std::collections::BTreeSet<String>,
) -> bool {
    if !visiting.insert(model.id.clone()) {
        return false;
    }

    let self_ready = is_model_self_ready(&models_dir.join(&model.id), model);
    let deps_ready = self_ready
        && model.dependencies.iter().all(|dependency_id| {
            registry
                .models
                .iter()
                .find(|entry| entry.id == *dependency_id)
                .map(|dependency| {
                    is_model_ready_with_registry_inner(models_dir, registry, dependency, visiting)
                })
                .unwrap_or(false)
        });

    visiting.remove(&model.id);
    deps_ready
}

fn collect_dependency_plan<'a>(
    registry: &'a ModelRegistry,
    model_id: &str,
) -> Result<Vec<&'a ModelInfo>> {
    fn visit<'a>(
        registry: &'a ModelRegistry,
        model_id: &str,
        visiting: &mut std::collections::BTreeSet<String>,
        visited: &mut std::collections::BTreeSet<String>,
        ordered: &mut Vec<&'a ModelInfo>,
    ) -> Result<()> {
        if visited.contains(model_id) {
            return Ok(());
        }
        if !visiting.insert(model_id.to_string()) {
            return Err(AppError::Model(format!(
                "Circular model dependency detected at '{}'",
                model_id
            )));
        }
        let model = registry
            .models
            .iter()
            .find(|entry| entry.id == model_id)
            .ok_or_else(|| {
                AppError::Model(format!("Model '{}' not found in registry", model_id))
            })?;
        for dependency_id in &model.dependencies {
            visit(registry, dependency_id, visiting, visited, ordered)?;
        }
        visiting.remove(model_id);
        visited.insert(model_id.to_string());
        ordered.push(model);
        Ok(())
    }

    let mut ordered = Vec::new();
    let mut visiting = std::collections::BTreeSet::new();
    let mut visited = std::collections::BTreeSet::new();
    visit(
        registry,
        model_id,
        &mut visiting,
        &mut visited,
        &mut ordered,
    )?;
    Ok(ordered)
}

fn dedupe_model_plan(plan: &mut Vec<&ModelInfo>) {
    let mut seen = std::collections::BTreeSet::new();
    plan.retain(|model| seen.insert(model.id.clone()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::AppPaths;

    fn test_registry() -> ModelRegistry {
        ModelRegistry {
            version: 1,
            updated_at: "2026-03-18".to_string(),
            models: vec![
                ModelInfo {
                    id: "fsmn-vad".to_string(),
                    name: "FSMN VAD".to_string(),
                    language: "zh".to_string(),
                    description: "shared vad".to_string(),
                    backend: ModelBackend::Auxiliary,
                    size_bytes: 10,
                    download_url: "https://example.com/fsmn-vad".to_string(),
                    sha256: String::new(),
                    version: "master".to_string(),
                    artifacts: vec![ModelArtifact {
                        role: ModelArtifactRole::ModelQuant,
                        file_name: "model_quant.onnx".to_string(),
                        size_bytes: 10,
                        sha256: String::new(),
                    }],
                    dependencies: Vec::new(),
                    is_downloaded: false,
                    download_path: None,
                },
                ModelInfo {
                    id: "ct-punc".to_string(),
                    name: "CT Punc".to_string(),
                    language: "zh,en".to_string(),
                    description: "shared punc".to_string(),
                    backend: ModelBackend::Auxiliary,
                    size_bytes: 10,
                    download_url: "https://example.com/ct-punc".to_string(),
                    sha256: String::new(),
                    version: "master".to_string(),
                    artifacts: vec![ModelArtifact {
                        role: ModelArtifactRole::Model,
                        file_name: "model.onnx".to_string(),
                        size_bytes: 10,
                        sha256: String::new(),
                    }],
                    dependencies: Vec::new(),
                    is_downloaded: false,
                    download_path: None,
                },
                ModelInfo {
                    id: "paraformer-zh-streaming".to_string(),
                    name: "Streaming".to_string(),
                    language: "zh".to_string(),
                    description: "live".to_string(),
                    backend: ModelBackend::Streaming,
                    size_bytes: 10,
                    download_url: "https://example.com/streaming".to_string(),
                    sha256: String::new(),
                    version: "master".to_string(),
                    artifacts: vec![
                        ModelArtifact {
                            role: ModelArtifactRole::Encoder,
                            file_name: "encoder.onnx".to_string(),
                            size_bytes: 4,
                            sha256: String::new(),
                        },
                        ModelArtifact {
                            role: ModelArtifactRole::Decoder,
                            file_name: "decoder.onnx".to_string(),
                            size_bytes: 4,
                            sha256: String::new(),
                        },
                        ModelArtifact {
                            role: ModelArtifactRole::Config,
                            file_name: "config.yaml".to_string(),
                            size_bytes: 1,
                            sha256: String::new(),
                        },
                        ModelArtifact {
                            role: ModelArtifactRole::Vocab,
                            file_name: "vocab.txt".to_string(),
                            size_bytes: 1,
                            sha256: String::new(),
                        },
                        ModelArtifact {
                            role: ModelArtifactRole::MeanVariance,
                            file_name: "am.mvn".to_string(),
                            size_bytes: 1,
                            sha256: String::new(),
                        },
                    ],
                    dependencies: vec!["fsmn-vad".to_string(), "ct-punc".to_string()],
                    is_downloaded: false,
                    download_path: None,
                },
            ],
        }
    }

    #[test]
    fn dependency_ready_requires_shared_models() {
        let dir = tempfile::tempdir().unwrap();
        let paths = AppPaths::new(dir.path().join("config"), dir.path().join("data"));
        paths.ensure_base_dirs().unwrap();

        let registry = test_registry();
        let models_dir = paths.models_dir();

        let streaming_dir = models_dir.join("paraformer-zh-streaming");
        std::fs::create_dir_all(&streaming_dir).unwrap();
        for file in [
            "encoder.onnx",
            "decoder.onnx",
            "config.yaml",
            "vocab.txt",
            "am.mvn",
        ] {
            std::fs::write(streaming_dir.join(file), b"x").unwrap();
        }

        let streaming = registry
            .models
            .iter()
            .find(|model| model.id == "paraformer-zh-streaming")
            .unwrap();
        assert!(!is_model_ready_with_registry(
            models_dir.as_path(),
            &registry,
            streaming
        ));

        let vad_dir = models_dir.join("fsmn-vad");
        std::fs::create_dir_all(&vad_dir).unwrap();
        std::fs::write(vad_dir.join("model_quant.onnx"), b"x").unwrap();
        assert!(!is_model_ready_with_registry(
            models_dir.as_path(),
            &registry,
            streaming
        ));

        let punc_dir = models_dir.join("ct-punc");
        std::fs::create_dir_all(&punc_dir).unwrap();
        std::fs::write(punc_dir.join("model.onnx"), b"x").unwrap();
        assert!(is_model_ready_with_registry(
            models_dir.as_path(),
            &registry,
            streaming
        ));
    }

    #[test]
    fn dependency_plan_downloads_shared_models_first() {
        let registry = test_registry();
        let ordered = collect_dependency_plan(&registry, "paraformer-zh-streaming").unwrap();
        let ids = ordered
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["fsmn-vad", "ct-punc", "paraformer-zh-streaming"]);
    }
}
