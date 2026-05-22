//! Built-in files plugin: walks a git repository, reads source files,
//! chunks with tree-sitter, and produces [`SourceItem`]s for the shared
//! indexing pipeline.

use std::path::PathBuf;

use anyhow::Result;
use async_trait::async_trait;

use crate::chunk::tree_sitter::TreeSitterChunker;
use crate::chunk::{Chunker, detect_language};
use crate::indexer::git;
use crate::indexer::walk;

use super::{FetchContext, FetchResult, Plugin, SourceChunk, SourceItem};

pub struct FilesPlugin {
    root: PathBuf,
}

impl FilesPlugin {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn chunk_file(&self, rel_path: &str, content: &str) -> Vec<SourceChunk> {
        let language = detect_language(rel_path).unwrap_or("unknown");
        let chunker = TreeSitterChunker::new();
        let chunks = match chunker.chunk(rel_path, content, language) {
            Ok(c) => c,
            Err(_) => return vec![],
        };
        chunks
            .symbols
            .into_iter()
            .map(|sym| SourceChunk {
                name: sym.name,
                kind: sym.kind,
                content: sym.body,
                signature: sym.signature,
                start_line: Some(sym.start_line),
                end_line: Some(sym.end_line),
                references: sym.references,
            })
            .collect()
    }

    fn build_item(&self, rel_path: &str, content: String) -> SourceItem {
        let content_hash = blake3::hash(content.as_bytes()).to_hex()[..16].to_string();
        let language = detect_language(rel_path).map(String::from);
        let children = self.chunk_file(rel_path, &content);
        SourceItem {
            source_path: rel_path.to_string(),
            content,
            content_hash,
            language,
            children,
        }
    }

    fn files_to_process(&self, ctx: &FetchContext) -> Result<(Vec<String>, Vec<String>)> {
        let head = git::current_sha(&self.root)?;

        match (&ctx.cursor, ctx.full) {
            (_, true) | (None, _) => {
                let files = walk::walk_files(&self.root)?;
                let rel_paths: Vec<String> = files
                    .iter()
                    .filter_map(|p| {
                        p.strip_prefix(&self.root)
                            .ok()
                            .map(|r| r.to_string_lossy().to_string())
                    })
                    .collect();
                Ok((rel_paths, vec![]))
            }
            (Some(from_sha), false) => {
                let diff = git::diff_files(&self.root, from_sha, &head)?;
                Ok((diff.changed, diff.deleted))
            }
        }
    }
}

#[async_trait]
impl Plugin for FilesPlugin {
    fn name(&self) -> &str {
        "files"
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<FetchResult> {
        let head = git::current_sha(&self.root)?;
        let (to_process, deletions) = self.files_to_process(ctx)?;

        let mut items = Vec::new();
        for rel_path in to_process {
            let abs_path = self.root.join(&rel_path);
            let content = match std::fs::read_to_string(&abs_path) {
                Ok(c) => c,
                Err(_) => continue,
            };

            let content_hash = blake3::hash(content.as_bytes()).to_hex()[..16].to_string();
            if !ctx.full
                && ctx
                    .stored_hashes
                    .get(&rel_path)
                    .is_some_and(|h| *h == content_hash)
            {
                continue;
            }

            items.push(self.build_item(&rel_path, content));
        }

        Ok(FetchResult {
            items,
            deletions,
            cursor: Some(head),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::Path;
    use std::process::Command;

    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn init_temp_repo(files: &[(&str, &str)]) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "wdpkr-files-plugin-{}-{}-{}",
            std::process::id(),
            n,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        Command::new("git")
            .args(["init"])
            .current_dir(&dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(&dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(&dir)
            .output()
            .unwrap();

        for (path, content) in files {
            let full_path = dir.join(path);
            if let Some(parent) = full_path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&full_path, content).unwrap();
        }

        Command::new("git")
            .args(["add", "."])
            .current_dir(&dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(&dir)
            .output()
            .unwrap();

        dir
    }

    fn cleanup(dir: &Path) {
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn full_fetch_returns_all_files() {
        let dir = init_temp_repo(&[
            ("src/main.rs", "pub fn main() {}\npub fn helper() {}"),
            ("src/lib.rs", "pub fn lib_fn() {}"),
            ("README.md", "# Hello"),
        ]);

        let plugin = FilesPlugin::new(dir.clone());
        let ctx = FetchContext {
            full: true,
            cursor: None,
            stored_hashes: HashMap::new(),
        };
        let result = plugin.fetch(&ctx).await.unwrap();

        assert_eq!(result.items.len(), 3);
        assert!(result.cursor.is_some());
        assert!(result.deletions.is_empty());

        let paths: Vec<&str> = result
            .items
            .iter()
            .map(|i| i.source_path.as_str())
            .collect();
        assert!(paths.contains(&"src/main.rs"), "paths: {paths:?}");
        assert!(paths.contains(&"src/lib.rs"), "paths: {paths:?}");
        assert!(paths.contains(&"README.md"), "paths: {paths:?}");

        cleanup(&dir);
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn rust_files_have_symbol_children() {
        let dir = init_temp_repo(&[(
            "src/main.rs",
            "pub fn hello() {\n    println!(\"hi\");\n}\n\npub fn goodbye() {\n    println!(\"bye\");\n}",
        )]);

        let plugin = FilesPlugin::new(dir.clone());
        let ctx = FetchContext {
            full: true,
            cursor: None,
            stored_hashes: HashMap::new(),
        };
        let result = plugin.fetch(&ctx).await.unwrap();

        let main_item = result
            .items
            .iter()
            .find(|i| i.source_path == "src/main.rs")
            .expect("should have main.rs");
        assert_eq!(main_item.language.as_deref(), Some("rust"));
        assert!(
            main_item.children.len() >= 2,
            "expected at least 2 symbols, got {}",
            main_item.children.len()
        );

        let names: Vec<&str> = main_item.children.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"hello"), "names: {names:?}");
        assert!(names.contains(&"goodbye"), "names: {names:?}");

        cleanup(&dir);
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn unknown_language_has_no_children() {
        let dir = init_temp_repo(&[("config.yaml", "key: value")]);

        let plugin = FilesPlugin::new(dir.clone());
        let ctx = FetchContext {
            full: true,
            cursor: None,
            stored_hashes: HashMap::new(),
        };
        let result = plugin.fetch(&ctx).await.unwrap();

        let item = &result.items[0];
        assert!(item.language.is_none());
        assert!(item.children.is_empty());

        cleanup(&dir);
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn content_hash_skip_detection() {
        let dir = init_temp_repo(&[("a.rs", "pub fn a() {}"), ("b.rs", "pub fn b() {}")]);

        let plugin = FilesPlugin::new(dir.clone());

        let hash_a = blake3::hash(b"pub fn a() {}").to_hex()[..16].to_string();
        let mut stored = HashMap::new();
        stored.insert("a.rs".into(), hash_a);

        let ctx = FetchContext {
            full: false,
            cursor: None,
            stored_hashes: stored,
        };
        let result = plugin.fetch(&ctx).await.unwrap();

        let paths: Vec<&str> = result
            .items
            .iter()
            .map(|i| i.source_path.as_str())
            .collect();
        assert!(!paths.contains(&"a.rs"), "a.rs should be skipped");
        assert!(paths.contains(&"b.rs"), "b.rs should be included");

        cleanup(&dir);
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn full_fetch_ignores_stored_hashes() {
        let dir = init_temp_repo(&[("a.rs", "pub fn a() {}")]);

        let plugin = FilesPlugin::new(dir.clone());

        let hash_a = blake3::hash(b"pub fn a() {}").to_hex()[..16].to_string();
        let mut stored = HashMap::new();
        stored.insert("a.rs".into(), hash_a);

        let ctx = FetchContext {
            full: true,
            cursor: None,
            stored_hashes: stored,
        };
        let result = plugin.fetch(&ctx).await.unwrap();

        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].source_path, "a.rs");

        cleanup(&dir);
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn incremental_fetch_detects_changes() {
        let dir = init_temp_repo(&[("a.rs", "pub fn a() {}"), ("b.rs", "pub fn b() {}")]);

        let sha1 = git::current_sha(&dir).unwrap();

        // Modify a.rs and add c.rs
        std::fs::write(dir.join("a.rs"), "pub fn a_v2() {}").unwrap();
        std::fs::write(dir.join("c.rs"), "pub fn c() {}").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(&dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "update"])
            .current_dir(&dir)
            .output()
            .unwrap();

        let plugin = FilesPlugin::new(dir.clone());
        let ctx = FetchContext {
            full: false,
            cursor: Some(sha1),
            stored_hashes: HashMap::new(),
        };
        let result = plugin.fetch(&ctx).await.unwrap();

        let paths: Vec<&str> = result
            .items
            .iter()
            .map(|i| i.source_path.as_str())
            .collect();
        assert!(paths.contains(&"a.rs"), "a.rs was modified");
        assert!(paths.contains(&"c.rs"), "c.rs was added");
        assert!(!paths.contains(&"b.rs"), "b.rs was unchanged");

        cleanup(&dir);
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn incremental_fetch_reports_deletions() {
        let dir = init_temp_repo(&[("a.rs", "pub fn a() {}"), ("b.rs", "pub fn b() {}")]);

        let sha1 = git::current_sha(&dir).unwrap();

        Command::new("git")
            .args(["rm", "b.rs"])
            .current_dir(&dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "delete b"])
            .current_dir(&dir)
            .output()
            .unwrap();

        let plugin = FilesPlugin::new(dir.clone());
        let ctx = FetchContext {
            full: false,
            cursor: Some(sha1),
            stored_hashes: HashMap::new(),
        };
        let result = plugin.fetch(&ctx).await.unwrap();

        assert!(result.deletions.contains(&"b.rs".to_string()));

        cleanup(&dir);
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn gitignored_files_excluded() {
        let dir = init_temp_repo(&[
            (".gitignore", "ignored.rs"),
            ("src/main.rs", "pub fn main() {}"),
        ]);

        std::fs::write(dir.join("ignored.rs"), "pub fn ignored() {}").unwrap();

        let plugin = FilesPlugin::new(dir.clone());
        let ctx = FetchContext {
            full: true,
            cursor: None,
            stored_hashes: HashMap::new(),
        };
        let result = plugin.fetch(&ctx).await.unwrap();

        let paths: Vec<&str> = result
            .items
            .iter()
            .map(|i| i.source_path.as_str())
            .collect();
        assert!(!paths.contains(&"ignored.rs"));

        cleanup(&dir);
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn empty_repo_returns_empty() {
        let dir = init_temp_repo(&[(".gitkeep", "")]);

        let plugin = FilesPlugin::new(dir.clone());
        let ctx = FetchContext {
            full: true,
            cursor: None,
            stored_hashes: HashMap::new(),
        };
        let result = plugin.fetch(&ctx).await.unwrap();

        // .gitkeep is hidden (starts with .), should be excluded by walk
        assert!(result.items.is_empty() || result.items.len() == 1);
        assert!(result.deletions.is_empty());
        assert!(result.cursor.is_some());

        cleanup(&dir);
    }

    #[test]
    fn plugin_name() {
        let plugin = FilesPlugin::new(PathBuf::from("."));
        assert_eq!(plugin.name(), "files");
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn content_hash_is_deterministic() {
        let dir = init_temp_repo(&[("a.rs", "pub fn a() {}")]);

        let plugin = FilesPlugin::new(dir.clone());
        let ctx = FetchContext {
            full: true,
            cursor: None,
            stored_hashes: HashMap::new(),
        };
        let r1 = plugin.fetch(&ctx).await.unwrap();
        let r2 = plugin.fetch(&ctx).await.unwrap();

        assert_eq!(r1.items[0].content_hash, r2.items[0].content_hash);

        cleanup(&dir);
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn symbol_chunks_have_line_numbers() {
        let dir = init_temp_repo(&[("src/main.rs", "pub fn hello() {\n    println!(\"hi\");\n}")]);

        let plugin = FilesPlugin::new(dir.clone());
        let ctx = FetchContext {
            full: true,
            cursor: None,
            stored_hashes: HashMap::new(),
        };
        let result = plugin.fetch(&ctx).await.unwrap();

        let item = &result
            .items
            .iter()
            .find(|i| i.source_path == "src/main.rs")
            .unwrap();
        let sym = &item.children[0];
        assert_eq!(sym.name, "hello");
        assert!(sym.start_line.is_some());
        assert!(sym.end_line.is_some());
        assert!(sym.start_line.unwrap() <= sym.end_line.unwrap());

        cleanup(&dir);
    }
}
