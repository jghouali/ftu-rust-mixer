use std::{
    collections::HashMap,
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppUserConfig {
    pub schema_version: u32,
    pub ain_aliases: HashMap<usize, String>,
    pub din_aliases: HashMap<usize, String>,
    pub out_aliases: HashMap<usize, String>,
}

impl Default for AppUserConfig {
    fn default() -> Self {
        Self {
            schema_version: 1,
            ain_aliases: HashMap::new(),
            din_aliases: HashMap::new(),
            out_aliases: HashMap::new(),
        }
    }
}

impl AppUserConfig {
    pub fn load_or_default() -> Result<Self> {
        let path = Self::config_file_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read config file {}", path.display()))?;
        let parsed = serde_json::from_str::<Self>(&text)
            .with_context(|| format!("Failed to parse config file {}", path.display()))?;
        Ok(parsed)
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::config_file_path()?;
        let dir = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Invalid config path {}", path.display()))?;
        fs::create_dir_all(dir)
            .with_context(|| format!("Failed to create config dir {}", dir.display()))?;
        let text = serde_json::to_string_pretty(self)?;
        fs::write(&path, text)
            .with_context(|| format!("Failed to write config file {}", path.display()))?;
        Ok(())
    }

    pub fn config_file_path() -> Result<PathBuf> {
        let home = env::var("HOME").context("HOME environment variable is not set")?;
        Ok(Path::new(&home).join(".ftu-mixer").join("config.json"))
    }
}
