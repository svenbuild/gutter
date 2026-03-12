use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub const APP_NAME: &str = "gutter";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct AppConfig {
    pub theme: String,
    pub tab_width: usize,
    pub line_numbers: bool,
    pub autosave: bool,
    pub autosave_ms: u64,
    pub show_hidden: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            theme: "base16-ocean.dark".to_string(),
            tab_width: 4,
            line_numbers: true,
            autosave: true,
            autosave_ms: 1_000,
            show_hidden: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ConfigLoad {
    pub config: AppConfig,
    pub warning: Option<String>,
}

impl AppConfig {
    pub fn load_or_default() -> ConfigLoad {
        let Some(path) = config_path() else {
            return ConfigLoad {
                config: Self::default(),
                warning: Some(
                    "Unable to resolve configuration directory; using defaults.".to_string(),
                ),
            };
        };

        match fs::read_to_string(&path) {
            Ok(contents) => match toml::from_str::<Self>(&contents) {
                Ok(config) => ConfigLoad {
                    config,
                    warning: None,
                },
                Err(error) => ConfigLoad {
                    config: Self::default(),
                    warning: Some(format!(
                        "Invalid config at {}: {error}. Using defaults.",
                        path.display()
                    )),
                },
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => ConfigLoad {
                config: Self::default(),
                warning: None,
            },
            Err(error) => ConfigLoad {
                config: Self::default(),
                warning: Some(format!(
                    "Unable to read config at {}: {error}. Using defaults.",
                    path.display()
                )),
            },
        }
    }
}

pub fn config_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join(APP_NAME))
}

pub fn config_path() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join("config.toml"))
}

pub fn session_path() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join("session.json"))
}

pub fn ensure_parent_dir(path: &PathBuf) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_matches_plan() {
        let config = AppConfig::default();
        assert_eq!(config.tab_width, 4);
        assert!(config.line_numbers);
        assert!(config.autosave);
        assert_eq!(config.autosave_ms, 1_000);
        assert!(!config.show_hidden);
    }
}
