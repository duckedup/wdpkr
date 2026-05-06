# chunk

AST-aware code chunking via tree-sitter.

## Purpose

Parses each indexable file into a file-level chunk plus zero or more
symbol-level chunks. Tree-sitter handles eight languages with symbol
granularity (Go, Rust, TS/JS, Python, Java, C/C++, C#); Svelte and unknown
languages get file-level only.

## Public surface

The trait and types are finalized in root `SPEC.md` § *Chunker trait*.

- `pub trait Chunker: Send + Sync`
  - `chunk(&self, file_path: &str, content: &str, language: &str) -> Result<FileChunks>`
- `pub struct FileChunks { file_path, language, imports, file_content, symbols }`
- `pub struct SymbolChunk { name, kind, body, signature, doc_comment, start_line, end_line }`
- `pub struct Import { module, names }`
- `pub struct TreeSitterChunker`

## Files

- `mod.rs` — trait + types + language detection (extension → language)
- `tree_sitter.rs` — generic AST walker; extracts symbols + imports + doc
  comments using the per-language config from `languages.rs`
- `languages.rs` — per-language: grammar handle, splittable node types,
  container nodes (e.g. Rust `impl_item`), comment node kinds, import node
  kinds + extraction logic

## Plan

Per root `SPEC.md` § *AST extraction strategy*:

1. Walk only the **root node's children** — top-level declarations only.
   Do not recurse into function bodies.
2. For container nodes (Rust `impl_item`, Java `class_declaration`, etc.),
   recurse one level to extract methods as separate symbol chunks.
3. **Doc comment association**: previous-sibling check for each extracted
   node; if it's a comment node, include in the chunk body.
4. **Import extraction**: at file level, collect all imports as
   `(module, names)` tuples — passed structured into the file summarizer
   prompt and stored as metadata on the file-level vector.
5. **Parse failure fallback**: tree-sitter parse failure → file-level-only
   chunking. Log to stderr, never block the indexing run.
6. **Oversized symbols** are passed through untouched here; truncation is
   the summarizer's problem.

Splittable node-type tables per language are in root `SPEC.md` §
*AST extraction strategy* and lifted into `languages.rs` directly.

## Open questions

- Splittable-node coverage validation against real codebases — root SPEC
  open Q #7. The map will need iteration.
- Language list adequacy — root SPEC open Q #4.
- `tree-sitter-svelte` is intentionally omitted in v1; revisit if usage
  warrants.
