use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config;

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionData {
    pub workspace: Option<PathBuf>,
    pub open_files: Vec<PathBuf>,
    pub active_file: Option<PathBuf>,
}

impl SessionData {
    pub fn load() -> anyhow::Result<Self> {
        let Some(path) = config::session_path() else {
            return Ok(Self::default());
        };

        match fs::read_to_string(&path) {
            Ok(contents) => Ok(serde_json::from_str::<Self>(&contents)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error.into()),
        }
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let Some(path) = config::session_path() else {
            return Ok(());
        };

        config::ensure_parent_dir(&path)?;
        fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn sanitize_for_workspace(&self, workspace: &Path) -> Self {
        let open_files = self
            .open_files
            .iter()
            .filter(|path| path.starts_with(workspace) && path.is_file())
            .cloned()
            .collect::<Vec<_>>();

        let active_file = self
            .active_file
            .clone()
            .filter(|path| path.starts_with(workspace) && path.is_file());

        Self {
            workspace: Some(workspace.to_path_buf()),
            open_files,
            active_file,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn sanitize_filters_outside_workspace() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("workspace");
        fs::create_dir_all(workspace.join("src")).unwrap();
        let file = workspace.join("src/main.rs");
        fs::write(&file, "fn main() {}").unwrap();
        let session = SessionData {
            workspace: Some(workspace.clone()),
            open_files: vec![file.clone(), PathBuf::from("C:/elsewhere/file.txt")],
            active_file: Some(PathBuf::from("C:/elsewhere/file.txt")),
        };

        let sanitized = session.sanitize_for_workspace(&workspace);
        assert_eq!(sanitized.open_files, vec![file]);
        assert_eq!(sanitized.active_file, None);
    }
}
