//! Non-LLM embed-text builders for docstring embed mode.
//!
//! These produce the text that gets embedded directly from code
//! documentation, with no summarizer in the loop. They are total functions:
//! a symbol with no docstring and no signature still yields a non-empty
//! string (its name), so indexing never fails on undocumented code.

use crate::tap::SourceChunk;

/// Char budget for the file-level "table of contents" embed text. Keeps a
/// huge generated file (hundreds of symbols) from producing an input that
/// exceeds the embedder's token limit. Dropped signatures still get their
/// own symbol-level vectors, so search recall is unaffected. Parallels the
/// big-file rollup guard in summary mode.
const MAX_TOC_CHARS: usize = 24_000;

/// Embed text for a single symbol: its cleaned docstring (if any) followed by
/// its signature. Never includes the body. Falls back to the signature alone
/// when undocumented, and to the symbol name when there is no signature.
pub fn symbol_embed_text(doc_comment: Option<&str>, signature: Option<&str>, name: &str) -> String {
    let doc = doc_comment.map(str::trim).filter(|s| !s.is_empty());
    let sig = signature.map(str::trim).filter(|s| !s.is_empty());
    let anchor = sig.unwrap_or(name);
    match doc {
        Some(d) => format!("{d}\n{anchor}"),
        None => anchor.to_string(),
    }
}

/// Embed text for a file-level "table of contents" vector: the file path, the
/// module-level doc comment (if any), and the signature of each symbol.
pub fn file_toc_text(
    file_path: &str,
    module_doc: Option<&str>,
    children: &[SourceChunk],
) -> String {
    let mut parts = vec![file_path.to_string()];
    let mut len = parts[0].len();
    if let Some(doc) = module_doc.map(str::trim).filter(|s| !s.is_empty()) {
        len += doc.len() + 1;
        parts.push(doc.to_string());
    }
    for child in children {
        let line = child
            .signature
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(child.name.as_str());
        if line.is_empty() {
            continue;
        }
        if len + line.len() + 1 > MAX_TOC_CHARS {
            break;
        }
        len += line.len() + 1;
        parts.push(line.to_string());
    }
    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(name: &str, signature: Option<&str>) -> SourceChunk {
        SourceChunk {
            name: name.into(),
            kind: "function".into(),
            content: String::new(),
            signature: signature.map(Into::into),
            doc_comment: None,
            start_line: None,
            end_line: None,
            references: vec![],
        }
    }

    #[test]
    fn symbol_with_doc_and_signature() {
        let text = symbol_embed_text(
            Some("Fetches a user by id."),
            Some("pub fn get_user(id: Uuid) -> Option<User>"),
            "get_user",
        );
        assert_eq!(
            text,
            "Fetches a user by id.\npub fn get_user(id: Uuid) -> Option<User>"
        );
    }

    #[test]
    fn symbol_without_doc_uses_signature_only() {
        let text = symbol_embed_text(None, Some("pub fn get_user(id: Uuid)"), "get_user");
        assert_eq!(text, "pub fn get_user(id: Uuid)");
    }

    #[test]
    fn symbol_without_doc_or_signature_falls_back_to_name() {
        // The critical no-documentation case: must not be empty, must not panic.
        let text = symbol_embed_text(None, None, "get_user");
        assert_eq!(text, "get_user");
        assert!(!text.is_empty());
    }

    #[test]
    fn symbol_empty_doc_and_signature_treated_as_absent() {
        let text = symbol_embed_text(Some("   "), Some("  "), "thing");
        assert_eq!(text, "thing");
    }

    #[test]
    fn file_toc_with_module_doc_and_signatures() {
        let children = vec![
            chunk("sign", Some("pub fn sign(claims: Claims) -> String")),
            chunk(
                "verify",
                Some("pub fn verify(token: &str) -> Result<Claims>"),
            ),
        ];
        let text = file_toc_text("src/auth/jwt.rs", Some("JWT helpers."), &children);
        assert_eq!(
            text,
            "src/auth/jwt.rs\nJWT helpers.\npub fn sign(claims: Claims) -> String\npub fn verify(token: &str) -> Result<Claims>"
        );
    }

    #[test]
    fn file_toc_without_module_doc() {
        let children = vec![chunk("main", Some("fn main()"))];
        let text = file_toc_text("src/main.rs", None, &children);
        assert_eq!(text, "src/main.rs\nfn main()");
    }

    #[test]
    fn file_toc_no_symbols_no_doc_is_just_path() {
        // A file with no docs and no symbols still produces a usable vector.
        let text = file_toc_text("config.txt", None, &[]);
        assert_eq!(text, "config.txt");
        assert!(!text.is_empty());
    }

    #[test]
    fn file_toc_symbol_without_signature_uses_name() {
        let children = vec![chunk("CONSTANT", None)];
        let text = file_toc_text("src/lib.rs", None, &children);
        assert_eq!(text, "src/lib.rs\nCONSTANT");
    }

    #[test]
    fn file_toc_caps_huge_files() {
        // Thousands of symbols must not produce an unbounded embed input.
        let sig = "pub fn some_reasonably_long_function_name(argument: SomeType) -> Result<()>";
        let children: Vec<SourceChunk> = (0..5000).map(|_| chunk("f", Some(sig))).collect();
        let text = file_toc_text("src/generated.rs", None, &children);
        assert!(text.len() <= MAX_TOC_CHARS, "len was {}", text.len());
        // The path is always retained even when truncating.
        assert!(text.starts_with("src/generated.rs"));
    }
}
