//! The game folder. Every path a game uses (in game.json, levels, scripts)
//! is relative to this folder and may not escape it.

use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context, Result};

#[derive(Debug, Clone)]
pub struct GameFs {
    root: PathBuf,
}

impl GameFs {
    pub fn new(root: &Path) -> Result<Self> {
        let root = root
            .canonicalize()
            .with_context(|| format!("game folder {} not found", root.display()))?;
        if !root.is_dir() {
            bail!("{} is not a folder", root.display());
        }
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Normalizes a game-relative path to `a/b/c.ext` form.
    /// Rejects absolute paths and `..` that would leave the game folder.
    pub fn normalize(path: &str) -> Result<String> {
        if path.starts_with('/') || path.starts_with('\\') || path.contains(':') {
            bail!("path '{path}' must be relative to the game folder");
        }
        let mut parts: Vec<&str> = Vec::new();
        for part in path.split(['/', '\\']) {
            match part {
                "" | "." => {}
                ".." => {
                    if parts.pop().is_none() {
                        bail!("path '{path}' escapes the game folder");
                    }
                }
                p => parts.push(p),
            }
        }
        if parts.is_empty() {
            bail!("empty path");
        }
        Ok(parts.join("/"))
    }

    /// Turns a game-relative path into a real file system path.
    pub fn resolve(&self, path: &str) -> Result<PathBuf> {
        Ok(self.root.join(Self::normalize(path)?))
    }

    pub fn read_string(&self, path: &str) -> Result<String> {
        let full = self.resolve(path)?;
        std::fs::read_to_string(&full).with_context(|| format!("failed to read '{path}'"))
    }

    pub fn exists(&self, path: &str) -> bool {
        self.resolve(path).map(|p| p.exists()).unwrap_or(false)
    }

    /// Lists the entries of a game folder directory (sorted, names only).
    pub fn list_dir(&self, path: &str) -> Result<Vec<String>> {
        let full = if path.is_empty() || path == "." {
            self.root.clone()
        } else {
            self.resolve(path)?
        };
        let mut names: Vec<String> = std::fs::read_dir(&full)
            .with_context(|| format!("failed to list '{path}'"))?
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        Ok(names)
    }

    /// Converts an absolute path inside the game folder back to a game path.
    pub fn relative(&self, abs: &Path) -> Option<String> {
        let rel = abs.strip_prefix(&self.root).ok()?;
        let parts: Vec<String> = rel
            .components()
            .filter_map(|c| match c {
                Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
                _ => None,
            })
            .collect();
        (!parts.is_empty()).then(|| parts.join("/"))
    }
}

#[cfg(test)]
mod tests {
    use super::GameFs;

    #[test]
    fn normalize_paths() {
        assert_eq!(
            GameFs::normalize("models/cube.gltf").unwrap(),
            "models/cube.gltf"
        );
        assert_eq!(
            GameFs::normalize("./models//x/../cube.gltf").unwrap(),
            "models/cube.gltf"
        );
        assert_eq!(
            GameFs::normalize("models\\cube.gltf").unwrap(),
            "models/cube.gltf"
        );
        assert!(GameFs::normalize("../secret").is_err());
        assert!(GameFs::normalize("a/../../b").is_err());
        assert!(GameFs::normalize("/etc/passwd").is_err());
        assert!(GameFs::normalize("C:/Windows").is_err());
        assert!(GameFs::normalize("").is_err());
    }
}
