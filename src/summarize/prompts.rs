//! Prompt templates for file-level and symbol-level summarization.
//!
//! These are the highest-leverage quality lever in the system. Changes
//! here should be gated on the eval harness. The target output is dense,
//! search-optimized prose that matches the vocabulary of user stories
//! and Linear tickets — NOT developer-facing documentation.

use crate::chunk::Import;

use super::{FileSummaryInput, SymbolSummaryInput};

pub const SYSTEM_PROMPT: &str = "\
You are a code summarizer for a semantic search index. Your summaries will be \
embedded as vectors and matched against natural-language queries — typically \
user stories, feature descriptions, or ticket titles like \"release commission \
payments to individual payees.\"\n\n\
Rules:\n\
- Write dense, specific prose. Every sentence should contain searchable terms.\n\
- Focus on WHAT the code does in business/domain terms, not HOW it's implemented.\n\
- Include key identifiers (function names, type names, API endpoints) naturally \
  in the prose — they're the bridge between code vocabulary and query vocabulary.\n\
- Reference relationships to other parts of the system when imports make them clear.\n\
- Do NOT include markdown formatting, bullet points, or headers. Plain prose only.\n\
- Do NOT start with \"This file\" or \"This function\" — start with the domain action.";

/// Build the user message for a file-level summarization call.
pub fn file_user_message(input: &FileSummaryInput) -> String {
    let mut msg = format!(
        "Summarize this {} file in 2-4 sentences.\n\nFile: {}\n",
        input.language, input.file_path
    );

    if !input.imports.is_empty() {
        msg.push_str("\nImports:\n");
        for import in &input.imports {
            msg.push_str(&format_import(import));
            msg.push('\n');
        }
    }

    msg.push_str("\nCode:\n");
    msg.push_str(&input.content);
    msg
}

/// Build the user message for a symbol-level summarization call.
pub fn symbol_user_message(input: &SymbolSummaryInput) -> String {
    let mut msg = format!(
        "Summarize this {} in 1-2 sentences.\n\n\
         File: {} — {}\n\n\
         {} {} ",
        input.symbol_kind,
        input.file_path,
        input.file_summary,
        input.symbol_kind,
        input.symbol_name,
    );

    if let Some(ref sig) = input.signature {
        msg.push_str(&format!("\nSignature: {sig}\n"));
    }

    if let Some(ref doc) = input.doc_comment {
        msg.push_str(&format!("\nDoc comment: {doc}\n"));
    }

    msg.push_str("\nBody:\n");
    msg.push_str(&input.body);
    msg
}

/// Build the user message for a roll-up file summary (used when the file
/// exceeds the token threshold and can't be passed directly).
pub fn rollup_user_message(
    file_path: &str,
    language: &str,
    symbol_summaries: &[(&str, &str)],
) -> String {
    let mut msg = format!(
        "Summarize this {language} file in 2-4 sentences based on its constituent symbols.\n\n\
         File: {file_path}\n\n\
         This file is too large to summarize directly. \
         Here are summaries of its individual symbols:\n\n"
    );
    for (name, summary) in symbol_summaries {
        msg.push_str(&format!("- {name}: {summary}\n"));
    }
    msg.push_str(
        "\nSynthesize these into a cohesive file-level summary. \
         Focus on the overall purpose and how the symbols relate to each other.",
    );
    msg
}

fn format_import(import: &Import) -> String {
    if import.names.is_empty() {
        format!("- {}", import.module)
    } else {
        format!("- {}: {}", import.module, import.names.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_file_input() -> FileSummaryInput {
        FileSummaryInput {
            file_path: "src/finance/commission/release.rs".into(),
            content: "pub fn release_payment(payee: &Payee) -> Result<()> { ... }".into(),
            imports: vec![
                Import {
                    module: "crate::models::Payee".into(),
                    names: vec!["Payee".into()],
                },
                Import {
                    module: "anyhow".into(),
                    names: vec!["Result".into()],
                },
            ],
            language: "rust".into(),
        }
    }

    fn sample_symbol_input() -> SymbolSummaryInput {
        SymbolSummaryInput {
            symbol_name: "release_payment".into(),
            symbol_kind: "function".into(),
            body: "pub fn release_payment(payee: &Payee, amount: f64) -> Result<()> { ... }".into(),
            signature: Some(
                "pub fn release_payment(payee: &Payee, amount: f64) -> Result<()>".into(),
            ),
            doc_comment: Some("Releases commission payment to the specified payee.".into()),
            file_path: "src/finance/commission/release.rs".into(),
            file_summary: "Commission payment release service with individual and batch support."
                .into(),
        }
    }

    #[test]
    fn system_prompt_is_non_empty() {
        assert!(!SYSTEM_PROMPT.is_empty());
        assert!(SYSTEM_PROMPT.contains("semantic search"));
    }

    #[test]
    fn file_message_includes_path_and_language() {
        let msg = file_user_message(&sample_file_input());
        assert!(msg.contains("src/finance/commission/release.rs"));
        assert!(msg.contains("rust"));
    }

    #[test]
    fn file_message_includes_imports() {
        let msg = file_user_message(&sample_file_input());
        assert!(msg.contains("crate::models::Payee"));
        assert!(msg.contains("Payee"));
        assert!(msg.contains("anyhow"));
    }

    #[test]
    fn file_message_includes_content() {
        let msg = file_user_message(&sample_file_input());
        assert!(msg.contains("release_payment"));
    }

    #[test]
    fn file_message_omits_imports_section_when_empty() {
        let input = FileSummaryInput {
            imports: vec![],
            ..sample_file_input()
        };
        let msg = file_user_message(&input);
        assert!(!msg.contains("Imports:"));
    }

    #[test]
    fn symbol_message_includes_file_summary_context() {
        let msg = symbol_user_message(&sample_symbol_input());
        assert!(msg.contains("Commission payment release service"));
    }

    #[test]
    fn symbol_message_includes_name_and_kind() {
        let msg = symbol_user_message(&sample_symbol_input());
        assert!(msg.contains("release_payment"));
        assert!(msg.contains("function"));
    }

    #[test]
    fn symbol_message_includes_signature() {
        let msg = symbol_user_message(&sample_symbol_input());
        assert!(msg.contains("pub fn release_payment(payee: &Payee, amount: f64)"));
    }

    #[test]
    fn symbol_message_includes_doc_comment() {
        let msg = symbol_user_message(&sample_symbol_input());
        assert!(msg.contains("Releases commission payment"));
    }

    #[test]
    fn symbol_message_omits_signature_when_none() {
        let input = SymbolSummaryInput {
            signature: None,
            ..sample_symbol_input()
        };
        let msg = symbol_user_message(&input);
        assert!(!msg.contains("Signature:"));
    }

    #[test]
    fn symbol_message_omits_doc_comment_when_none() {
        let input = SymbolSummaryInput {
            doc_comment: None,
            ..sample_symbol_input()
        };
        let msg = symbol_user_message(&input);
        assert!(!msg.contains("Doc comment:"));
    }

    #[test]
    fn format_import_with_names() {
        let import = Import {
            module: "std::collections".into(),
            names: vec!["HashMap".into(), "BTreeMap".into()],
        };
        assert_eq!(
            format_import(&import),
            "- std::collections: HashMap, BTreeMap"
        );
    }

    #[test]
    fn format_import_without_names() {
        let import = Import {
            module: "fmt".into(),
            names: vec![],
        };
        assert_eq!(format_import(&import), "- fmt");
    }
}
