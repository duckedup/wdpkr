//! Repo walker: enumerate indexable files respecting .gitignore and .wdpkrignore.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

const BINARY_EXTENSIONS: &[&str] = &[
    // Images
    "png", "jpg", "jpeg", "gif", "bmp", "ico", "svg", "webp", "tiff", "tif", "avif",
    // Audio/video
    "mp3", "mp4", "wav", "ogg", "flac", "avi", "mov", "mkv", "webm", // Archives
    "zip", "tar", "gz", "bz2", "xz", "7z", "rar", "zst", // Compiled/binary
    "wasm", "pyc", "pyo", "class", "o", "a", "so", "dylib", "dll", "exe", // Documents
    "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", // Fonts
    "ttf", "otf", "woff", "woff2", "eot", // Data blobs
    "sqlite", "db", "bin", "dat",
];

fn is_binary_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| BINARY_EXTENSIONS.contains(&ext.to_lowercase().as_str()))
}

/// Walk the repo from `root`, returning paths to all indexable files.
/// Respects .gitignore and .wdpkrignore automatically.
pub fn walk_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut builder = ignore::WalkBuilder::new(root);
    builder
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true);

    let ignore_path = root.join(".wdpkrignore");
    if ignore_path.exists() {
        builder.add_ignore(&ignore_path);
    }

    let walker = builder.build();

    for entry in walker {
        let entry = entry.context("walking repo")?;
        if entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            let path = entry.into_path();
            if !is_binary_extension(&path) {
                files.push(path);
            }
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

    #[test]
    fn binary_extension_detection() {
        assert!(is_binary_extension(Path::new("favicon.png")));
        assert!(is_binary_extension(Path::new("logo.PNG")));
        assert!(is_binary_extension(Path::new("app.wasm")));
        assert!(is_binary_extension(Path::new("archive.tar.gz")));
        assert!(!is_binary_extension(Path::new("main.rs")));
        assert!(!is_binary_extension(Path::new("README.md")));
        assert!(!is_binary_extension(Path::new("config.yaml")));
    }

    #[test]
    fn excludes_binary_files() {
        let files = walk_files(Path::new(".")).unwrap();
        let has_binary = files.iter().any(|p| is_binary_extension(p));
        assert!(!has_binary, "should not include binary files");
    }
}
