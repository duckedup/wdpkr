//! Repo walker: enumerate indexable files respecting .gitignore.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Walk the repo from `root`, returning paths to all indexable files.
/// Respects .gitignore automatically via the `ignore` crate.
pub fn walk_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let walker = ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .build();

    for entry in walker {
        let entry = entry.context("walking repo")?;
        if entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            files.push(entry.into_path());
        }
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walks_current_repo() {
        let files = walk_files(Path::new(".")).unwrap();
        assert!(!files.is_empty());
        let has_cargo = files.iter().any(|p| p.ends_with("Cargo.toml"));
        assert!(has_cargo, "should find Cargo.toml");
    }

    #[test]
    fn excludes_git_dir() {
        let files = walk_files(Path::new(".")).unwrap();
        let has_git = files
            .iter()
            .any(|p| p.components().any(|c| c.as_os_str() == ".git"));
        assert!(!has_git, "should not include .git directory contents");
    }

    #[test]
    fn excludes_target_dir() {
        let files = walk_files(Path::new(".")).unwrap();
        let has_target = files
            .iter()
            .any(|p| p.components().any(|c| c.as_os_str() == "target"));
        assert!(!has_target, "should not include target/ (gitignored)");
    }
}
