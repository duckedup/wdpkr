//! AST-aware code chunking.
//!
//! Parses each indexable file into a file-level chunk plus zero or more
//! symbol-level chunks. The [`Chunker`] trait abstracts the parsing
//! strategy; the v1 implementation uses tree-sitter for 8 languages
//! with symbol granularity.
//!
//! Implementation tracks root `SPEC.md` § Chunker trait and § AST
//! extraction strategy.

pub mod languages;
pub mod tree_sitter;

/// A chunker parses source files into a file-level chunk and zero or more
/// symbol-level chunks. The trait is synchronous — parsing is CPU-bound
/// and fast enough to not warrant async.
pub trait Chunker: Send + Sync {
    fn chunk(&self, file_path: &str, content: &str, language: &str) -> anyhow::Result<FileChunks>;
}

/// The result of chunking a single file.
#[derive(Debug, Clone)]
pub struct FileChunks {
    pub file_path: String,
    pub language: String,
    /// Full file content, passed to the file-level summarizer.
    pub file_content: String,
    /// Structured imports — `(module, imported_names)` tuples. Passed to
    /// the file-level summarizer as context and stored as metadata on the
    /// file-level vector.
    pub imports: Vec<Import>,
    /// Symbol-level chunks extracted from the AST. Empty if the language
    /// has no grammar or if parsing failed (fallback to file-level only).
    pub symbols: Vec<SymbolChunk>,
}

/// A single symbol extracted from the AST: a function, method, struct,
/// trait, enum, etc. Includes the full body (with any leading doc comment)
/// plus metadata the summarizer and indexer need.
#[derive(Debug, Clone)]
pub struct SymbolChunk {
    /// The identifier — function name, type name, etc.
    pub name: String,
    /// Normalized kind: `function`, `method`, `struct`, `enum`, `trait`,
    /// `type`, `interface`, `const`, `class`.
    pub kind: String,
    /// Full text of the node, including any leading doc comment.
    pub body: String,
    /// For functions/methods: just the signature line(s), used in summaries
    /// for cross-referencing.
    pub signature: Option<String>,
    /// Extracted doc comment text (if a comment node precedes this symbol
    /// in the AST).
    pub doc_comment: Option<String>,
    /// 1-based line range in the source file.
    pub start_line: u32,
    pub end_line: u32,
}

/// A structured import extracted from the AST.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Import {
    /// Module path, e.g. `"std::collections"` or `"internal/finance/commission"`.
    pub module: String,
    /// Specific names imported, e.g. `["HashMap", "BTreeMap"]`.
    /// Empty if the whole module is imported.
    pub names: Vec<String>,
}

/// Detect language from a file extension. Returns `None` for unrecognized
/// extensions — the caller can still produce a file-level-only chunk
/// without a grammar.
pub fn detect_language(file_path: &str) -> Option<&'static str> {
    let ext = file_path.rsplit('.').next()?;
    match ext {
        "rs" => Some("rust"),
        "go" => Some("go"),
        "ts" => Some("typescript"),
        "tsx" => Some("tsx"),
        "js" | "jsx" => Some("javascript"),
        "py" => Some("python"),
        "java" => Some("java"),
        "c" | "h" => Some("c"),
        "cc" | "cpp" | "cxx" | "hpp" | "hxx" | "hh" => Some("cpp"),
        "cs" => Some("csharp"),
        "svelte" => Some("svelte"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn _assert_send<T: Send>() {}
    fn _assert_sync<T: Sync>() {}

    #[test]
    fn trait_is_object_safe() {
        fn _takes_chunker(_: &dyn Chunker) {}
    }

    // ── detect_language ───────────────────────────────────────────────

    #[test]
    fn detects_rust() {
        assert_eq!(detect_language("src/main.rs"), Some("rust"));
    }

    #[test]
    fn detects_go() {
        assert_eq!(detect_language("pkg/handler.go"), Some("go"));
    }

    #[test]
    fn detects_typescript() {
        assert_eq!(detect_language("src/app.ts"), Some("typescript"));
    }

    #[test]
    fn detects_tsx() {
        assert_eq!(detect_language("src/App.tsx"), Some("tsx"));
    }

    #[test]
    fn detects_javascript() {
        assert_eq!(detect_language("index.js"), Some("javascript"));
        assert_eq!(detect_language("App.jsx"), Some("javascript"));
    }

    #[test]
    fn detects_python() {
        assert_eq!(detect_language("main.py"), Some("python"));
    }

    #[test]
    fn detects_java() {
        assert_eq!(detect_language("Service.java"), Some("java"));
    }

    #[test]
    fn detects_c_and_cpp() {
        assert_eq!(detect_language("main.c"), Some("c"));
        assert_eq!(detect_language("util.h"), Some("c"));
        assert_eq!(detect_language("main.cpp"), Some("cpp"));
        assert_eq!(detect_language("main.cc"), Some("cpp"));
        assert_eq!(detect_language("util.hpp"), Some("cpp"));
    }

    #[test]
    fn detects_csharp() {
        assert_eq!(detect_language("Program.cs"), Some("csharp"));
    }

    #[test]
    fn detects_svelte() {
        assert_eq!(detect_language("App.svelte"), Some("svelte"));
    }

    #[test]
    fn unknown_extension_returns_none() {
        assert_eq!(detect_language("Makefile"), None);
        assert_eq!(detect_language("data.json"), None);
        assert_eq!(detect_language("README.md"), None);
    }

    #[test]
    fn handles_nested_paths() {
        assert_eq!(
            detect_language("internal/finance/commission/release.go"),
            Some("go")
        );
    }

    #[test]
    fn handles_dotfiles() {
        assert_eq!(detect_language(".gitignore"), None);
    }

    // ── Type construction ─────────────────────────────────────────────

    #[test]
    fn import_equality() {
        let a = Import {
            module: "std::collections".into(),
            names: vec!["HashMap".into()],
        };
        let b = Import {
            module: "std::collections".into(),
            names: vec!["HashMap".into()],
        };
        assert_eq!(a, b);
    }

    #[test]
    fn file_chunks_with_no_symbols() {
        let chunks = FileChunks {
            file_path: "README.md".into(),
            language: "markdown".into(),
            file_content: "# Hello".into(),
            imports: vec![],
            symbols: vec![],
        };
        assert!(chunks.symbols.is_empty());
        assert!(chunks.imports.is_empty());
    }

    #[test]
    fn symbol_chunk_fields() {
        let sym = SymbolChunk {
            name: "process_payment".into(),
            kind: "function".into(),
            body: "pub fn process_payment(amount: f64) -> Result<()> { ... }".into(),
            signature: Some("pub fn process_payment(amount: f64) -> Result<()>".into()),
            doc_comment: Some("Processes a payment for the given amount.".into()),
            start_line: 42,
            end_line: 78,
        };
        assert_eq!(sym.name, "process_payment");
        assert_eq!(sym.kind, "function");
        assert!(sym.signature.is_some());
        assert!(sym.doc_comment.is_some());
        assert_eq!(sym.start_line, 42);
        assert_eq!(sym.end_line, 78);
    }
}
