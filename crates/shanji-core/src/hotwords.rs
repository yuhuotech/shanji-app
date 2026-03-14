use crate::error::{AppError, Result};
use crate::paths::AppPaths;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hotword {
    pub word: String,
    pub weight: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HotwordLibrary {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub format: LibraryFormat,
    pub word_count: usize,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LibraryFormat {
    Txt,
    Csv,
    Scel,
    Thuocl,
}

impl std::fmt::Display for LibraryFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LibraryFormat::Txt => write!(f, "txt"),
            LibraryFormat::Csv => write!(f, "csv"),
            LibraryFormat::Scel => write!(f, "scel"),
            LibraryFormat::Thuocl => write!(f, "thuocl"),
        }
    }
}

pub fn list_libraries_with_paths(paths: &AppPaths) -> Result<Vec<HotwordLibrary>> {
    let libraries = load_libraries_with_paths(paths)?;
    let mut libs: Vec<_> = libraries.into_values().collect();
    libs.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Ok(libs)
}

pub fn get_library_with_paths(paths: &AppPaths, id: &str) -> Result<Option<HotwordLibrary>> {
    let libraries = load_libraries_with_paths(paths)?;
    Ok(libraries.get(id).cloned())
}

pub fn load_words_with_paths(paths: &AppPaths, library_id: &str) -> Result<Vec<Hotword>> {
    let libraries = load_libraries_with_paths(paths)?;
    let library = libraries
        .get(library_id)
        .ok_or_else(|| AppError::InvalidInput(format!("Library not found: {}", library_id)))?;

    let file_path = get_library_file_path_with_paths(paths, library_id, library.format.clone());
    if !file_path.exists() {
        return Ok(vec![]);
    }

    let content = std::fs::read(&file_path)
        .map_err(|e| AppError::Io(format!("Failed to read library file: {}", e)))?;

    match library.format {
        LibraryFormat::Txt | LibraryFormat::Thuocl => {
            let text = String::from_utf8(content)
                .map_err(|e| AppError::InvalidInput(format!("Invalid UTF-8: {}", e)))?;
            Ok(parse_txt(&text))
        }
        LibraryFormat::Csv => {
            let text = String::from_utf8(content)
                .map_err(|e| AppError::InvalidInput(format!("Invalid UTF-8: {}", e)))?;
            parse_csv(&text)
        }
        LibraryFormat::Scel => parse_scel(&content),
    }
}

pub fn load_all_enabled_with_paths(paths: &AppPaths) -> Result<Vec<Hotword>> {
    let libraries = load_libraries_with_paths(paths)?;
    let mut all_words = Vec::new();

    for (id, library) in libraries {
        if library.enabled {
            let file_path = get_library_file_path_with_paths(paths, &id, library.format.clone());
            let content = match std::fs::read(&file_path) {
                Ok(content) => content,
                Err(e) => {
                    log::warn!("Failed to read hotword library file {}: {}", id, e);
                    continue;
                }
            };

            let result = match library.format {
                LibraryFormat::Txt | LibraryFormat::Thuocl => String::from_utf8(content)
                    .map(|text| parse_txt(&text))
                    .map_err(|e| AppError::InvalidInput(format!("Invalid UTF-8: {}", e))),
                LibraryFormat::Csv => String::from_utf8(content)
                    .map_err(|e| AppError::InvalidInput(format!("Invalid UTF-8: {}", e)))
                    .and_then(|text| parse_csv(&text)),
                LibraryFormat::Scel => parse_scel(&content),
            };

            match result {
                Ok(words) => all_words.extend(words),
                Err(e) => log::warn!("Failed to load hotwords from {}: {}", id, e),
            }
        }
    }

    Ok(all_words)
}

pub fn create_library_with_paths(
    paths: &AppPaths,
    name: String,
    format: LibraryFormat,
    content: &str,
) -> Result<HotwordLibrary> {
    let id = format!(
        "lib_{}",
        uuid::Uuid::new_v4().to_string().replace("-", "")[..16].to_string()
    );
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    let words = match format {
        LibraryFormat::Txt | LibraryFormat::Thuocl => parse_txt(content),
        LibraryFormat::Csv => parse_csv(content)?,
        LibraryFormat::Scel => {
            return Err(AppError::InvalidInput(
                "SCel format requires binary file upload".to_string(),
            ))
        }
    };

    let word_count = words.len();
    let file_path = get_library_file_path_with_paths(paths, &id, format.clone());
    std::fs::write(&file_path, content)
        .map_err(|e| AppError::Io(format!("Failed to write library file: {}", e)))?;

    let library = HotwordLibrary {
        id: id.clone(),
        name,
        enabled: true,
        format,
        word_count,
        created_at: now,
        updated_at: now,
    };

    let mut libraries = load_libraries_with_paths(paths)?;
    libraries.insert(id, library.clone());
    save_libraries_with_paths(paths, &libraries)?;
    Ok(library)
}

pub fn import_library_file_with_paths(
    paths: &AppPaths,
    name: String,
    format: LibraryFormat,
    data: Vec<u8>,
) -> Result<HotwordLibrary> {
    let id = format!(
        "lib_{}",
        uuid::Uuid::new_v4().to_string().replace("-", "")[..16].to_string()
    );
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    let words = match format {
        LibraryFormat::Scel => parse_scel(&data)?,
        _ => {
            return Err(AppError::InvalidInput(
                "Binary import only supported for SCel format".to_string(),
            ))
        }
    };

    let word_count = words.len();
    let file_path = get_library_file_path_with_paths(paths, &id, format.clone());
    std::fs::write(&file_path, data)
        .map_err(|e| AppError::Io(format!("Failed to write library file: {}", e)))?;

    let library = HotwordLibrary {
        id: id.clone(),
        name,
        enabled: true,
        format,
        word_count,
        created_at: now,
        updated_at: now,
    };

    let mut libraries = load_libraries_with_paths(paths)?;
    libraries.insert(id, library.clone());
    save_libraries_with_paths(paths, &libraries)?;
    Ok(library)
}

pub fn set_library_enabled_with_paths(paths: &AppPaths, id: &str, enabled: bool) -> Result<()> {
    let mut libraries = load_libraries_with_paths(paths)?;
    let library = libraries
        .get_mut(id)
        .ok_or_else(|| AppError::InvalidInput(format!("Library not found: {}", id)))?;

    library.enabled = enabled;
    library.updated_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    save_libraries_with_paths(paths, &libraries)
}

pub fn delete_library_with_paths(paths: &AppPaths, id: &str) -> Result<()> {
    let mut libraries = load_libraries_with_paths(paths)?;
    let library = libraries
        .remove(id)
        .ok_or_else(|| AppError::InvalidInput(format!("Library not found: {}", id)))?;

    let file_path = get_library_file_path_with_paths(paths, id, library.format);
    if file_path.exists() {
        let _ = std::fs::remove_file(file_path);
    }

    save_libraries_with_paths(paths, &libraries)
}

fn get_hotwords_dir_from_paths(paths: &AppPaths) -> PathBuf {
    paths.hotwords_dir()
}

fn get_library_meta_path(paths: &AppPaths) -> PathBuf {
    get_hotwords_dir_from_paths(paths).join("libraries.json")
}

fn load_libraries_with_paths(paths: &AppPaths) -> Result<HashMap<String, HotwordLibrary>> {
    let meta_path = get_library_meta_path(paths);
    if !meta_path.exists() {
        return Ok(HashMap::new());
    }

    let content = std::fs::read_to_string(&meta_path)
        .map_err(|e| AppError::Io(format!("Failed to read libraries: {}", e)))?;
    let libraries: Vec<HotwordLibrary> = serde_json::from_str(&content)
        .map_err(|e| AppError::Config(format!("Failed to parse libraries: {}", e)))?;

    Ok(libraries
        .into_iter()
        .map(|lib| (lib.id.clone(), lib))
        .collect())
}

fn save_libraries_with_paths(
    paths: &AppPaths,
    libraries: &HashMap<String, HotwordLibrary>,
) -> Result<()> {
    let meta_path = get_library_meta_path(paths);
    if let Some(parent) = meta_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AppError::Io(format!("Failed to create dir: {}", e)))?;
    }

    let libs: Vec<_> = libraries.values().collect();
    let json = serde_json::to_string_pretty(&libs)
        .map_err(|e| AppError::Config(format!("Failed to serialize libraries: {}", e)))?;
    std::fs::write(&meta_path, json)
        .map_err(|e| AppError::Io(format!("Failed to write libraries: {}", e)))?;
    Ok(())
}

fn get_library_file_path_with_paths(
    paths: &AppPaths,
    library_id: &str,
    format: LibraryFormat,
) -> PathBuf {
    let hotwords_dir = get_hotwords_dir_from_paths(paths);
    let ext = format.to_string();
    hotwords_dir.join(format!("{}.{}", library_id, ext))
}

pub fn parse_txt(content: &str) -> Vec<Hotword> {
    content
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.is_empty() {
                return None;
            }
            let word = parts[0].to_string();
            let weight = parts
                .get(1)
                .and_then(|w| w.parse().ok())
                .unwrap_or(10)
                .clamp(1, 100);
            Some(Hotword { word, weight })
        })
        .collect()
}

pub fn parse_csv(content: &str) -> Result<Vec<Hotword>> {
    let mut words = Vec::new();
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .from_reader(content.as_bytes());

    for result in reader.records() {
        let record =
            result.map_err(|e| AppError::InvalidInput(format!("CSV parse error: {}", e)))?;
        let word = record
            .get(0)
            .ok_or_else(|| AppError::InvalidInput("Missing word column".to_string()))?
            .to_string();
        let weight = record
            .get(1)
            .and_then(|w| w.parse().ok())
            .unwrap_or(10)
            .clamp(1, 100);
        words.push(Hotword { word, weight });
    }

    Ok(words)
}

pub fn parse_scel(bytes: &[u8]) -> Result<Vec<Hotword>> {
    if bytes.len() < 0x200 {
        return Err(AppError::InvalidInput(
            "Invalid SCel file: too small".to_string(),
        ));
    }
    if &bytes[0..4] != b"\x40\x15\x00\x00" {
        return Err(AppError::InvalidInput(
            "Invalid SCel file: wrong magic number".to_string(),
        ));
    }

    let mut words = Vec::new();
    let mut i = 0x200;
    while i + 2 < bytes.len() {
        let py_len = u16::from_le_bytes([bytes[i], bytes[i + 1]]) as usize;
        i += 2;
        if i + py_len > bytes.len() {
            break;
        }
        i += py_len;
        if i + 2 > bytes.len() {
            break;
        }
        let word_len = u16::from_le_bytes([bytes[i], bytes[i + 1]]) as usize;
        i += 2;
        if i + word_len > bytes.len() {
            break;
        }

        if word_len % 2 == 0 {
            let word_bytes = &bytes[i..i + word_len];
            let u16_iter = (0..word_bytes.len())
                .step_by(2)
                .map(|j| u16::from_le_bytes([word_bytes[j], word_bytes[j + 1]]));

            if let Ok(word) = String::from_utf16(&u16_iter.collect::<Vec<_>>()) {
                if !word.is_empty() && word.chars().all(|c| !c.is_control()) {
                    words.push(Hotword {
                        word: word.trim().to_string(),
                        weight: 10,
                    });
                }
            }
        }

        i += word_len;
    }

    if words.is_empty() {
        return Err(AppError::InvalidInput(
            "No valid words found in SCel file".to_string(),
        ));
    }

    Ok(words)
}

pub fn parse_thuocl(content: &str) -> Vec<Hotword> {
    parse_txt(content)
}
