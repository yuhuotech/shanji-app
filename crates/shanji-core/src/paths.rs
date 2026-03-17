use crate::error::Result;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct AppPaths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
}

impl AppPaths {
    pub fn new(config_dir: PathBuf, data_dir: PathBuf) -> Self {
        Self {
            config_dir,
            data_dir,
        }
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.json")
    }

    pub fn models_dir(&self) -> PathBuf {
        self.data_dir.join("models")
    }

    pub fn hotwords_dir(&self) -> PathBuf {
        self.data_dir.join("hotwords")
    }

    pub fn builtin_hotwords_dir(&self) -> PathBuf {
        self.hotwords_dir().join("builtin")
    }

    pub fn user_hotwords_dir(&self) -> PathBuf {
        self.hotwords_dir().join("user")
    }

    pub fn imported_hotwords_dir(&self) -> PathBuf {
        self.hotwords_dir().join("imported")
    }

    pub fn temp_dir(&self) -> PathBuf {
        self.data_dir.join("temp")
    }

    pub fn recordings_dir(&self) -> PathBuf {
        self.data_dir.join("recordings")
    }

    pub fn ensure_base_dirs(&self) -> Result<()> {
        std::fs::create_dir_all(&self.config_dir)?;
        std::fs::create_dir_all(&self.data_dir)?;
        std::fs::create_dir_all(self.models_dir())?;
        std::fs::create_dir_all(self.hotwords_dir())?;
        std::fs::create_dir_all(self.builtin_hotwords_dir())?;
        std::fs::create_dir_all(self.user_hotwords_dir())?;
        std::fs::create_dir_all(self.imported_hotwords_dir())?;
        std::fs::create_dir_all(self.temp_dir())?;
        std::fs::create_dir_all(self.recordings_dir())?;
        Ok(())
    }
}

pub fn standard_app_paths(app_name: &str) -> Result<AppPaths> {
    let config_dir = dirs::config_dir()
        .ok_or_else(|| {
            crate::error::AppError::Io("Failed to resolve config directory".to_string())
        })?
        .join(app_name);
    let data_dir = dirs::data_dir()
        .ok_or_else(|| crate::error::AppError::Io("Failed to resolve data directory".to_string()))?
        .join(app_name);

    Ok(AppPaths::new(config_dir, data_dir))
}
