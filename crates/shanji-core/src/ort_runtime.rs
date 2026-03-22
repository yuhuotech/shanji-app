use crate::error::{AppError, Result};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

static ORT_INIT_RESULT: OnceLock<Result<()>> = OnceLock::new();

pub fn init_onnx_runtime() -> Result<()> {
    if let Some(result) = ORT_INIT_RESULT.get() {
        return result.clone();
    }

    let dylib_path = locate_onnx_runtime_dylib()?;
    let builder = ort::init_from(&dylib_path).map_err(|err| {
        AppError::Internal(format!(
            "Failed to create ONNX Runtime environment: {}",
            err
        ))
    })?;
    let result = if !builder.commit() {
        log::info!("ONNX Runtime already initialized");
        Ok(())
    } else {
        log::info!("Initialized ONNX Runtime from {}", dylib_path.display());
        Ok(())
    };

    let _ = ORT_INIT_RESULT.set(result);
    ORT_INIT_RESULT
        .get()
        .cloned()
        .unwrap_or_else(|| Err(AppError::Internal("Failed to cache ORT init result".to_string())))
}

fn locate_onnx_runtime_dylib() -> Result<PathBuf> {
    if let Ok(path) = env::var("ORT_DYLIB_PATH") {
        let path = PathBuf::from(path);
        if path.exists() {
            return Ok(path);
        }
    }

    let exe = env::current_exe().map_err(|err| {
        AppError::Internal(format!("Failed to resolve current executable: {}", err))
    })?;

    for ancestor in exe.ancestors() {
        for candidate in candidate_paths(ancestor) {
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }

    Err(AppError::NotFound(
        "ONNX Runtime dylib not found. Set ORT_DYLIB_PATH or bundle libonnxruntime.dylib next to the app.".to_string(),
    ))
}

fn candidate_paths(base: &Path) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    candidates.extend(find_dylibs_in(base));
    candidates.extend(find_dylibs_in(&base.join("Frameworks")));
    candidates
}

fn find_dylibs_in(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };

    entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.starts_with("libonnxruntime") && name.ends_with(".dylib"))
                .unwrap_or(false)
        })
        .collect()
}
