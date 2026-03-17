use crate::error::{AppError, Result};
use crate::paths::AppPaths;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

const BUILTIN_LIBRARIES: &[(&str, &str)] = &[
    (
        "builtin/ai-coding.md",
        include_str!("../assets/hotwords/ai-coding.md"),
    ),
    (
        "builtin/computing.md",
        include_str!("../assets/hotwords/computing.md"),
    ),
    (
        "builtin/cross-border-ecommerce.md",
        include_str!("../assets/hotwords/cross-border-ecommerce.md"),
    ),
    (
        "builtin/self-media.md",
        include_str!("../assets/hotwords/self-media.md"),
    ),
    (
        "builtin/internet-slang.md",
        include_str!("../assets/hotwords/internet-slang.md"),
    ),
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hotword {
    pub word: String,
    pub weight: i32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Default)]
#[serde(rename_all = "lowercase")]
pub enum LibrarySource {
    Builtin,
    #[default]
    User,
    Imported,
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
    #[serde(default)]
    pub source: LibrarySource,
    #[serde(default)]
    pub relative_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LibraryFormat {
    Md,
    Txt,
    Csv,
    Scel,
    Thuocl,
}

impl std::fmt::Display for LibraryFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LibraryFormat::Md => write!(f, "md"),
            LibraryFormat::Txt => write!(f, "txt"),
            LibraryFormat::Csv => write!(f, "csv"),
            LibraryFormat::Scel => write!(f, "scel"),
            LibraryFormat::Thuocl => write!(f, "thuocl"),
        }
    }
}

#[derive(Debug, Default, Clone)]
struct MarkdownMeta {
    id: Option<String>,
    name: Option<String>,
    enabled: Option<bool>,
    weight: Option<i32>,
}

pub fn ensure_default_libraries_with_paths(paths: &AppPaths) -> Result<()> {
    for (relative_path, content) in BUILTIN_LIBRARIES {
        let path = paths.hotwords_dir().join(relative_path);
        if path.exists() {
            continue;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| AppError::Io(format!("Failed to create hotword dir: {}", e)))?;
        }
        std::fs::write(&path, content)
            .map_err(|e| AppError::Io(format!("Failed to write built-in hotwords: {}", e)))?;
    }
    Ok(())
}

pub fn list_libraries_with_paths(paths: &AppPaths) -> Result<Vec<HotwordLibrary>> {
    let libraries = load_libraries_with_paths(paths)?;
    let mut libs: Vec<_> = libraries.into_values().collect();
    libs.sort_by(|a, b| {
        b.enabled
            .cmp(&a.enabled)
            .then(a.source.cmp(&b.source))
            .then(a.name.cmp(&b.name))
    });
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
    let file_path = library_file_path(paths, library);
    load_words_from_file(&file_path, &library.format)
}

pub fn load_all_enabled_with_paths(paths: &AppPaths) -> Result<Vec<Hotword>> {
    let libraries = load_libraries_with_paths(paths)?;
    let mut merged = HashMap::<String, Hotword>::new();

    for library in libraries.values().filter(|library| library.enabled) {
        match load_words_from_file(&library_file_path(paths, library), &library.format) {
            Ok(words) => {
                for word in words {
                    if word.word.trim().is_empty() {
                        continue;
                    }
                    let key = word.word.to_ascii_lowercase();
                    merged
                        .entry(key)
                        .and_modify(|existing| {
                            if word.weight > existing.weight {
                                *existing = word.clone();
                            }
                        })
                        .or_insert(word);
                }
            }
            Err(err) => {
                log::warn!("Failed to load hotwords from {}: {}", library.id, err);
            }
        }
    }

    let mut words = merged.into_values().collect::<Vec<_>>();
    words.sort_by(|a, b| b.weight.cmp(&a.weight).then(a.word.cmp(&b.word)));
    Ok(words)
}

pub fn create_library_with_paths(
    paths: &AppPaths,
    name: String,
    format: LibraryFormat,
    content: &str,
) -> Result<HotwordLibrary> {
    let id = format!("user_{}", uuid::Uuid::new_v4().simple());
    let relative_path = format!("user/{}.{}", id, format);
    let file_path = paths.hotwords_dir().join(&relative_path);
    if let Some(parent) = file_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AppError::Io(format!("Failed to create hotword dir: {}", e)))?;
    }
    std::fs::write(&file_path, content)
        .map_err(|e| AppError::Io(format!("Failed to write library file: {}", e)))?;

    let words = load_words_from_file(&file_path, &format)?;
    let now = now_millis();
    let library = HotwordLibrary {
        id: id.clone(),
        name,
        enabled: true,
        format,
        word_count: words.len(),
        created_at: now,
        updated_at: now,
        source: LibrarySource::User,
        relative_path,
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
    let id = format!("imported_{}", uuid::Uuid::new_v4().simple());
    let relative_path = format!("imported/{}.{}", id, format);
    let file_path = paths.hotwords_dir().join(&relative_path);
    if let Some(parent) = file_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AppError::Io(format!("Failed to create hotword dir: {}", e)))?;
    }
    std::fs::write(&file_path, data)
        .map_err(|e| AppError::Io(format!("Failed to write library file: {}", e)))?;

    let words = load_words_from_file(&file_path, &format)?;
    let now = now_millis();
    let library = HotwordLibrary {
        id: id.clone(),
        name,
        enabled: true,
        format,
        word_count: words.len(),
        created_at: now,
        updated_at: now,
        source: LibrarySource::Imported,
        relative_path,
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
    library.updated_at = now_millis();
    save_libraries_with_paths(paths, &libraries)
}

pub fn delete_library_with_paths(paths: &AppPaths, id: &str) -> Result<()> {
    let mut libraries = load_libraries_with_paths(paths)?;
    let library = libraries
        .remove(id)
        .ok_or_else(|| AppError::InvalidInput(format!("Library not found: {}", id)))?;

    if library.source != LibrarySource::Builtin {
        let file_path = library_file_path(paths, &library);
        if file_path.exists() {
            let _ = std::fs::remove_file(file_path);
        }
    }

    save_libraries_with_paths(paths, &libraries)
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn load_libraries_with_paths(paths: &AppPaths) -> Result<HashMap<String, HotwordLibrary>> {
    let saved = load_saved_libraries(paths)?;
    let mut libraries = HashMap::new();
    let mut changed = false;

    for discovered in scan_library_files(paths)? {
        let merged = if let Some(existing) = saved.get(&discovered.id) {
            HotwordLibrary {
                enabled: existing.enabled,
                created_at: existing.created_at,
                updated_at: existing.updated_at.max(discovered.updated_at),
                ..discovered
            }
        } else {
            changed = true;
            discovered
        };
        libraries.insert(merged.id.clone(), merged);
    }

    if changed || libraries.len() != saved.len() {
        save_libraries_with_paths(paths, &libraries)?;
    }

    Ok(libraries)
}

fn load_saved_libraries(paths: &AppPaths) -> Result<HashMap<String, HotwordLibrary>> {
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
        .map(|library| (library.id.clone(), library))
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

    let mut libs: Vec<_> = libraries.values().cloned().collect();
    libs.sort_by(|a, b| a.name.cmp(&b.name));
    let json = serde_json::to_string_pretty(&libs)
        .map_err(|e| AppError::Config(format!("Failed to serialize libraries: {}", e)))?;
    std::fs::write(&meta_path, json)
        .map_err(|e| AppError::Io(format!("Failed to write libraries: {}", e)))?;
    Ok(())
}

fn scan_library_files(paths: &AppPaths) -> Result<Vec<HotwordLibrary>> {
    let hotwords_dir = paths.hotwords_dir();
    if !hotwords_dir.exists() {
        return Ok(Vec::new());
    }

    let mut libraries = Vec::new();
    let mut seen_ids = HashSet::new();
    for entry in WalkDir::new(&hotwords_dir)
        .into_iter()
        .filter_map(|entry| entry.ok())
    {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(format) = library_format_from_path(path) else {
            continue;
        };

        let relative_path = path
            .strip_prefix(&hotwords_dir)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        let source = library_source_from_relative_path(&relative_path);
        let (meta, words) = load_words_with_meta_from_file(path, &format)?;
        let id = meta
            .id
            .clone()
            .unwrap_or_else(|| sanitize_library_id(&relative_path));
        if !seen_ids.insert(id.clone()) {
            log::warn!("Duplicate hotword library id detected, skipping {}", id);
            continue;
        }

        let name = meta
            .name
            .unwrap_or_else(|| default_library_name(path, source));
        let now = now_millis();
        libraries.push(HotwordLibrary {
            id,
            name,
            enabled: meta.enabled.unwrap_or(source == LibrarySource::Builtin),
            format,
            word_count: words.len(),
            created_at: now,
            updated_at: now,
            source,
            relative_path,
        });
    }

    Ok(libraries)
}

fn load_words_with_meta_from_file(
    path: &Path,
    format: &LibraryFormat,
) -> Result<(MarkdownMeta, Vec<Hotword>)> {
    match format {
        LibraryFormat::Md => {
            let text = std::fs::read_to_string(path)
                .map_err(|e| AppError::Io(format!("Failed to read markdown library: {}", e)))?;
            parse_markdown(&text)
        }
        LibraryFormat::Txt | LibraryFormat::Thuocl => {
            let text = std::fs::read_to_string(path)
                .map_err(|e| AppError::Io(format!("Failed to read library file: {}", e)))?;
            Ok((MarkdownMeta::default(), parse_txt(&text)))
        }
        LibraryFormat::Csv => {
            let text = std::fs::read_to_string(path)
                .map_err(|e| AppError::Io(format!("Failed to read csv library: {}", e)))?;
            Ok((MarkdownMeta::default(), parse_csv(&text)?))
        }
        LibraryFormat::Scel => {
            let bytes = std::fs::read(path)
                .map_err(|e| AppError::Io(format!("Failed to read scel library: {}", e)))?;
            Ok((MarkdownMeta::default(), parse_scel(&bytes)?))
        }
    }
}

fn load_words_from_file(path: &Path, format: &LibraryFormat) -> Result<Vec<Hotword>> {
    load_words_with_meta_from_file(path, format).map(|(_, words)| words)
}

fn library_file_path(paths: &AppPaths, library: &HotwordLibrary) -> PathBuf {
    if !library.relative_path.is_empty() {
        return paths.hotwords_dir().join(&library.relative_path);
    }
    paths
        .hotwords_dir()
        .join(format!("{}.{}", library.id, library.format))
}

fn get_library_meta_path(paths: &AppPaths) -> PathBuf {
    paths.hotwords_dir().join("libraries.json")
}

fn library_source_from_relative_path(relative_path: &str) -> LibrarySource {
    if relative_path.starts_with("builtin/") {
        LibrarySource::Builtin
    } else if relative_path.starts_with("imported/") {
        LibrarySource::Imported
    } else {
        LibrarySource::User
    }
}

fn library_format_from_path(path: &Path) -> Option<LibraryFormat> {
    match path
        .extension()?
        .to_string_lossy()
        .to_ascii_lowercase()
        .as_str()
    {
        "md" => Some(LibraryFormat::Md),
        "txt" => Some(LibraryFormat::Txt),
        "csv" => Some(LibraryFormat::Csv),
        "scel" => Some(LibraryFormat::Scel),
        "thuocl" => Some(LibraryFormat::Thuocl),
        _ => None,
    }
}

fn sanitize_library_id(relative_path: &str) -> String {
    relative_path
        .trim_end_matches(".md")
        .trim_end_matches(".txt")
        .trim_end_matches(".csv")
        .trim_end_matches(".scel")
        .trim_end_matches(".thuocl")
        .replace('/', "_")
        .replace('\\', "_")
        .replace(' ', "_")
        .replace('-', "_")
}

fn default_library_name(path: &Path, source: LibrarySource) -> String {
    let stem = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_string())
        .unwrap_or_else(|| "词典".to_string());
    match source {
        LibrarySource::Builtin => format!("内置 · {}", prettify_file_name(&stem)),
        LibrarySource::Imported => format!("导入 · {}", prettify_file_name(&stem)),
        LibrarySource::User => prettify_file_name(&stem),
    }
}

fn prettify_file_name(name: &str) -> String {
    name.replace(['-', '_'], " ")
}

fn parse_markdown(content: &str) -> Result<(MarkdownMeta, Vec<Hotword>)> {
    let (meta, body) = parse_markdown_meta(content);
    let default_weight = meta.weight.unwrap_or(70).clamp(1, 100);
    let mut words = Vec::new();

    for raw_line in body.lines() {
        let line = raw_line.trim();
        if line.is_empty()
            || line.starts_with('#')
            || line.starts_with("```")
            || line.starts_with('>')
        {
            continue;
        }

        let normalized = line
            .trim_start_matches("- ")
            .trim_start_matches("* ")
            .trim_start_matches("+ ")
            .trim();
        if normalized.is_empty() {
            continue;
        }

        let word = parse_weighted_markdown_word(normalized, default_weight);
        if let Some(word) = word {
            words.push(word);
        }
    }

    Ok((meta, dedupe_hotwords(words)))
}

fn parse_markdown_meta(content: &str) -> (MarkdownMeta, &str) {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---\n") && !trimmed.starts_with("---\r\n") {
        return (MarkdownMeta::default(), content);
    }

    let separator = if trimmed.starts_with("---\r\n") {
        "\r\n---"
    } else {
        "\n---"
    };
    let Some(end_idx) = trimmed[4..].find(separator) else {
        return (MarkdownMeta::default(), content);
    };
    let frontmatter = &trimmed[4..4 + end_idx];
    let body_start = 4 + end_idx + separator.len();
    let body = &trimmed[body_start..].trim_start_matches(['\r', '\n']);
    let mut meta = MarkdownMeta::default();

    for line in frontmatter.lines() {
        let mut parts = line.splitn(2, ':');
        let Some(key) = parts.next().map(str::trim) else {
            continue;
        };
        let Some(value) = parts.next().map(str::trim) else {
            continue;
        };
        let value = value.trim_matches('"').trim_matches('\'');
        match key {
            "id" => meta.id = Some(value.to_string()),
            "name" => meta.name = Some(value.to_string()),
            "enabled" => meta.enabled = Some(matches!(value, "true" | "yes" | "on")),
            "weight" => meta.weight = value.parse::<i32>().ok(),
            _ => {}
        }
    }

    (meta, body)
}

fn parse_weighted_markdown_word(line: &str, default_weight: i32) -> Option<Hotword> {
    if let Some((word, weight)) = line.split_once('|') {
        return Some(Hotword {
            word: word.trim().to_string(),
            weight: weight
                .trim()
                .parse::<i32>()
                .ok()
                .unwrap_or(default_weight)
                .clamp(1, 100),
        });
    }

    if let Some((word, weight)) = line.rsplit_once(' ') {
        if let Ok(weight) = weight.trim().parse::<i32>() {
            return Some(Hotword {
                word: word.trim().to_string(),
                weight: weight.clamp(1, 100),
            });
        }
    }

    Some(Hotword {
        word: line.trim().to_string(),
        weight: default_weight,
    })
}

fn dedupe_hotwords(words: Vec<Hotword>) -> Vec<Hotword> {
    let mut merged = HashMap::<String, Hotword>::new();
    for word in words {
        if word.word.is_empty() {
            continue;
        }
        let key = word.word.to_ascii_lowercase();
        merged
            .entry(key)
            .and_modify(|existing| {
                if word.weight > existing.weight {
                    *existing = word.clone();
                }
            })
            .or_insert(word);
    }
    merged.into_values().collect()
}

pub fn parse_txt(content: &str) -> Vec<Hotword> {
    dedupe_hotwords(
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
            .collect(),
    )
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
            .trim()
            .to_string();
        let weight = record
            .get(1)
            .and_then(|w| w.trim().parse().ok())
            .unwrap_or(10)
            .clamp(1, 100);
        words.push(Hotword { word, weight });
    }

    Ok(dedupe_hotwords(words))
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
                let trimmed = word.trim();
                if !trimmed.is_empty() && trimmed.chars().all(|c| !c.is_control()) {
                    words.push(Hotword {
                        word: trimmed.to_string(),
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

    Ok(dedupe_hotwords(words))
}

pub fn parse_thuocl(content: &str) -> Vec<Hotword> {
    parse_txt(content)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_dictionary_parses_frontmatter_and_bullets() {
        let content = r#"---
id: ai_coding
name: AI 编程
enabled: true
weight: 88
---

# AI 编程

- OpenAI
- ChatGPT | 92
- Cursor 90
"#;
        let (meta, words) = parse_markdown(content).unwrap();
        assert_eq!(meta.id.as_deref(), Some("ai_coding"));
        assert_eq!(meta.name.as_deref(), Some("AI 编程"));
        assert_eq!(words.len(), 3);
        assert!(words
            .iter()
            .any(|word| word.word == "ChatGPT" && word.weight == 92));
    }
}
