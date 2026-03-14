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
    Ok(all_models
        .into_iter()
        .filter(|model| model.is_downloaded)
        .collect())
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

/// Download and install the model, calling `progress_fn(downloaded_bytes, total_bytes)`.
/// Tries multiple sources: modelscope → huggingface (individual files) → github (tar.gz).
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

    let urls = collect_download_urls(model);
    if urls.is_empty() {
        return Err(AppError::Model(format!(
            "No download URLs configured for model '{}'",
            model_id
        )));
    }

    let client = build_blocking_client()?;
    let models_dir = paths.models_dir();
    std::fs::create_dir_all(&models_dir)?;
    let model_dir = get_model_dir_with_paths(paths, model_id);
    std::fs::create_dir_all(&model_dir)?;
    let temp_archive = models_dir.join(format!(".{}.downloading.tar.gz", model_id));

    let mut last_error: Option<String> = None;
    let mut used_archive = false;

    for url in &urls {
        let is_archive = url.ends_with(".tar.gz");
        used_archive = is_archive;

        // For archive downloads: keep any partial temp file for resume — do NOT delete it here.
        // For individual-file downloads: restart each file from scratch on retry.

        let result = if is_archive {
            download_archive(&client, url, &temp_archive, model.size_bytes, &progress_fn)
        } else {
            download_individual_files(
                &client,
                url,
                &model_dir,
                model_id,
                model.size_bytes,
                &model.files,
                &progress_fn,
            )
        };

        match result {
            Ok(()) => {
                last_error = None;
                break;
            }
            Err(e) => {
                eprintln!("[shanji] Download from {} failed: {}", url, e);
                last_error = Some(e.to_string());
            }
        }
    }

    if let Some(e) = last_error {
        return Err(AppError::Network(format!(
            "All download sources failed. Last error: {}",
            e
        )));
    }

    // Extract archive if we downloaded a tar.gz
    if used_archive && temp_archive.exists() {
        extract_tar_gz(&temp_archive, &model_dir)?;
        let _ = std::fs::remove_file(&temp_archive);

        // Flatten if archive extracted into a subdirectory
        if !is_model_ready(&model_dir, &model.files) {
            flatten_single_subdirectory(&model_dir)?;
        }
    }

    // tokens.txt → vocab.txt alias (for tokenizer compatibility)
    let tokens_path = model_dir.join("tokens.txt");
    let vocab_path = model_dir.join("vocab.txt");
    if tokens_path.exists() && !vocab_path.exists() {
        std::fs::rename(&tokens_path, &vocab_path)
            .map_err(|e| AppError::Io(format!("Failed to rename tokens.txt: {}", e)))?;
    }

    validate_model_dir(&model_dir, &model.files)?;
    Ok(())
}

/// Build list of download URLs to try in priority order: modelscope → huggingface → primary.
fn collect_download_urls(model: &ModelInfo) -> Vec<String> {
    let mut urls = Vec::new();
    if let Some(ref u) = model.modelscope_url {
        if !u.is_empty() {
            urls.push(u.clone());
        }
    }
    if let Some(ref u) = model.huggingface_url {
        if !u.is_empty() {
            urls.push(u.clone());
        }
    }
    if !model.download_url.is_empty() {
        urls.push(model.download_url.clone());
    }
    urls
}

fn build_blocking_client() -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(10))
        .connect_timeout(std::time::Duration::from_secs(30))
        // No overall timeout — we handle read stalls ourselves via recv_timeout
        .build()
        .map_err(|e| AppError::Network(format!("Failed to build HTTP client: {}", e)))
}

/// Download a tar.gz archive to `dest` with:
/// - **Resume support**: if `dest` already exists, sends a `Range` header to continue.
/// - **Stall detection**: if no bytes arrive for `STALL_TIMEOUT_SECS`, returns an error
///   immediately while keeping the partial file so the next call can resume.
fn download_archive<F>(
    client: &reqwest::blocking::Client,
    url: &str,
    dest: &Path,
    expected_size: u64,
    progress_fn: &F,
) -> Result<()>
where
    F: Fn(u64, u64),
{
    use std::io::Write;
    use std::sync::mpsc;

    const STALL_TIMEOUT_SECS: u64 = 60;

    // Resume: check how many bytes we already have
    let start_byte = if dest.exists() {
        std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0)
    } else {
        0
    };

    let mut request = client.get(url);
    if start_byte > 0 {
        eprintln!(
            "[shanji] Resuming archive download from byte {}",
            start_byte
        );
        request = request.header("Range", format!("bytes={}-", start_byte));
    }

    let response = request
        .send()
        .map_err(|e| AppError::Network(format!("Request failed: {}", e)))?;

    let status = response.status();
    let resuming = start_byte > 0 && status == reqwest::StatusCode::PARTIAL_CONTENT;

    if !status.is_success() && !resuming {
        return Err(AppError::Network(format!("HTTP {} from {}", status, url)));
    }

    let content_len = response.content_length().unwrap_or(0);
    let total = if resuming {
        start_byte + content_len
    } else {
        content_len.max(expected_size)
    };

    // Open file: append if resuming, truncate if fresh start
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(resuming)
        .truncate(!resuming)
        .write(true)
        .open(dest)
        .map_err(|e| AppError::Io(format!("Cannot open temp file: {}", e)))?;

    // Spawn a dedicated reader thread so we can apply a recv_timeout (stall detection)
    // without the blocking read() hanging the whole download thread forever.
    let (tx, rx) = mpsc::channel::<Result<Vec<u8>>>();
    let mut body = response;
    std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = vec![0u8; 16_384]; // 16 KB — frequent progress updates
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
                    let _ = tx.send(Err(AppError::Io(format!("Read error: {}", e))));
                    break;
                }
            }
        }
    });

    let timeout = std::time::Duration::from_secs(STALL_TIMEOUT_SECS);
    let mut downloaded = if resuming { start_byte } else { 0 };

    loop {
        match rx.recv_timeout(timeout) {
            Ok(Ok(chunk)) if chunk.is_empty() => break, // EOF — done
            Ok(Ok(chunk)) => {
                file.write_all(&chunk)
                    .map_err(|e| AppError::Io(format!("Write error: {}", e)))?;
                downloaded += chunk.len() as u64;
                progress_fn(downloaded, total);
            }
            Ok(Err(e)) => return Err(e),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // Partial file is preserved on disk — caller can retry and resume
                return Err(AppError::Network(format!(
                    "Download stalled: no data for {} seconds (downloaded {} / {} bytes)",
                    STALL_TIMEOUT_SECS, downloaded, total
                )));
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    Ok(())
}

/// Download individual model files from a HuggingFace-style base URL.
/// Files listed in `file_names` (e.g. `model_quant.onnx`, `asr.yaml`, …) are fetched as
/// `{base_url}{filename}`.  Missing config files get a generated default so the download
/// can still succeed.
fn download_individual_files<F>(
    client: &reqwest::blocking::Client,
    base_url: &str,
    model_dir: &Path,
    model_id: &str,
    total_size_hint: u64,
    file_names: &[String],
    progress_fn: &F,
) -> Result<()>
where
    F: Fn(u64, u64),
{
    use std::io::Write;

    let base = if base_url.ends_with('/') {
        base_url.to_string()
    } else {
        format!("{}/", base_url)
    };

    // Determine which files to fetch and their approximate sizes
    let default_files: Vec<(&str, u64)> = vec![
        ("model.onnx", 880_000_000),
        ("asr.yaml", 10_000),
        ("am.mvn", 12_000),
        ("tokens.txt", 35_000),
    ];

    let files: Vec<(String, u64)> = if file_names.is_empty() {
        default_files
            .iter()
            .map(|(n, s)| (n.to_string(), *s))
            .collect()
    } else {
        file_names
            .iter()
            .map(|n| {
                let size = default_files
                    .iter()
                    .find(|(dn, _)| *dn == n.as_str())
                    .map(|(_, s)| *s)
                    .unwrap_or(50_000);
                (n.clone(), size)
            })
            .collect()
    };

    let total: u64 = files.iter().map(|(_, s)| s).sum();
    let total = total.max(total_size_hint);
    let mut downloaded_total = 0u64;

    for (filename, expected_size) in &files {
        let file_url = format!("{}{}", base, filename);
        let dest = model_dir.join(filename);

        eprintln!("[shanji] Downloading {} from {}", filename, file_url);

        let response_result = client.get(&file_url).send();
        match response_result {
            Ok(response) if response.status().is_success() => {
                use std::sync::mpsc;
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
                let mut file = std::fs::File::create(&dest)
                    .map_err(|e| AppError::Io(format!("Cannot create {}: {}", filename, e)))?;
                let timeout = std::time::Duration::from_secs(60);
                loop {
                    match rx.recv_timeout(timeout) {
                        Ok(Ok(chunk)) if chunk.is_empty() => break,
                        Ok(Ok(chunk)) => {
                            file.write_all(&chunk).map_err(|e| {
                                AppError::Io(format!("Write error for {}: {}", filename, e))
                            })?;
                            downloaded_total += chunk.len() as u64;
                            progress_fn(downloaded_total.min(total), total);
                        }
                        Ok(Err(e)) => {
                            return Err(AppError::Io(format!("Read error for {}: {}", filename, e)))
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            return Err(AppError::Network(format!(
                                "Stalled while downloading {}: no data for 60 seconds",
                                filename
                            )))
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
            }
            Ok(response) => {
                let status = response.status();
                // Optional config file: generate a default rather than hard-failing
                if filename == "asr.yaml" || filename == "config.yaml" {
                    eprintln!(
                        "[shanji] {} not found (HTTP {}), generating default",
                        filename, status
                    );
                    let default_cfg = "model_type: paraformer\nmodel_file: model_quant.onnx\nsampling_rate: 16000\nlanguage: zh\n";
                    std::fs::write(&dest, default_cfg).map_err(|e| {
                        AppError::Io(format!("Cannot write default {}: {}", filename, e))
                    })?;
                    downloaded_total += expected_size;
                    progress_fn(downloaded_total.min(total), total);
                } else {
                    return Err(AppError::Network(format!(
                        "HTTP {} when downloading {} from {}",
                        status, filename, file_url
                    )));
                }
            }
            Err(e) => {
                if filename == "asr.yaml" || filename == "config.yaml" {
                    eprintln!(
                        "[shanji] {} download error ({}), generating default",
                        filename, e
                    );
                    let default_cfg = "model_type: paraformer\nmodel_file: model_quant.onnx\nsampling_rate: 16000\nlanguage: zh\n";
                    std::fs::write(&dest, default_cfg).map_err(|e2| {
                        AppError::Io(format!("Cannot write default {}: {}", filename, e2))
                    })?;
                    downloaded_total += expected_size;
                    progress_fn(downloaded_total.min(total), total);
                } else {
                    return Err(AppError::Network(format!(
                        "Request failed for {}: {}",
                        filename, e
                    )));
                }
            }
        }
    }

    let _ = model_id;
    Ok(())
}

/// Extract a tar.gz archive into `dest_dir`.
fn extract_tar_gz(archive_path: &Path, dest_dir: &Path) -> Result<()> {
    let file = std::fs::File::open(archive_path)
        .map_err(|e| AppError::Io(format!("Cannot open archive: {}", e)))?;
    let gz = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(gz);
    archive
        .unpack(dest_dir)
        .map_err(|e| AppError::Io(format!("Extraction failed: {}", e)))?;
    Ok(())
}

/// Move all files from the only immediate subdirectory of `dir` up into `dir` itself.
fn flatten_single_subdirectory(dir: &Path) -> Result<()> {
    let subdirs: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| AppError::Io(e.to_string()))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .collect();

    if subdirs.len() != 1 {
        return Ok(());
    }

    let sub = subdirs[0].path();
    for entry in std::fs::read_dir(&sub).map_err(|e| AppError::Io(e.to_string()))? {
        let entry = entry.map_err(|e| AppError::Io(e.to_string()))?;
        let dest = dir.join(entry.file_name());
        std::fs::rename(entry.path(), dest).map_err(|e| AppError::Io(e.to_string()))?;
    }
    let _ = std::fs::remove_dir_all(&sub);
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
