//! Non-LLM embed-text builders for **prose** items (document taps like notion
//! and linear, plus prose files such as `.md`/`.txt` in the files tap).
//!
//! In docstring embed mode there is no summarizer in the loop. For code, the
//! embed text is the docstring + signature (see [`super::docstring`]). For
//! prose there is no such metadata, so the natural no-LLM embed text is the raw
//! prose itself — the document leads with its title and each section carries its
//! own content, both of which embed well directly.
//!
//! Whether an item is code or prose is decided structurally by
//! [`has_code_structure`], not by extension or tap: a code item always carries
//! signatures from the chunker, so anything without them (a Notion page, a
//! Linear issue, a repo README) takes the prose path.

use crate::tap::SourceChunk;

/// Char budget for a prose document's embed text (file-level and per-section),
/// keeping a long spec from exceeding the embedder's input limit.
const MAX_PROSE_CHARS: usize = 24_000;

/// Whether an item carries code structure (a module doc or any child with a
/// signature). Code items get the TOC/signature treatment; items without it are
/// prose documents that embed their raw text.
pub fn has_code_structure(module_doc: Option<&str>, children: &[SourceChunk]) -> bool {
    module_doc.is_some_and(|d| !d.trim().is_empty())
        || children
            .iter()
            .any(|c| c.signature.as_deref().is_some_and(|s| !s.trim().is_empty()))
}

/// Char budget for the concise display summary stored on a prose document's
/// file-level and section vectors. Keeps search output readable — the full
/// prose is still what gets embedded.
const MAX_SUMMARY_CHARS: usize = 320;

/// Embed text for a prose document's file-level vector: its rendered content
/// (which already leads with the title), truncated to a safe input size.
pub fn prose_doc_text(content: &str) -> String {
    truncate_chars(content.trim(), MAX_PROSE_CHARS)
}

/// Concise **display** summary for a prose document's file-level result: the
/// title line plus the first body sentence/line, capped. This is what search
/// shows for the whole-doc hit — the detail lives in the section results.
pub fn prose_doc_summary(content: &str) -> String {
    let title = content
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| l.trim_start_matches('#').trim())
        .unwrap_or_default();
    let lead = content
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with("URL:"))
        .unwrap_or_default();
    let combined = if lead.is_empty() || lead == title {
        title.to_string()
    } else {
        format!("{title}\n{lead}")
    };
    snippet(&combined, MAX_SUMMARY_CHARS)
}

/// Embed text for a prose section: its heading followed by its content (the
/// heading adds retrieval context), truncated. Falls back to the heading alone
/// when the body is empty.
pub fn prose_section_text(name: &str, content: &str) -> String {
    let body = content.trim();
    let text = if body.is_empty() {
        name.trim().to_string()
    } else {
        format!("{}\n{body}", name.trim())
    };
    truncate_chars(&text, MAX_PROSE_CHARS)
}

/// Concise **display** summary for a prose section result: its content, capped.
/// The heading is shown separately in the result header, so it's omitted here;
/// an empty body falls back to the heading.
pub fn prose_section_summary(name: &str, content: &str) -> String {
    let body = content.trim();
    let text = if body.is_empty() { name.trim() } else { body };
    snippet(text, MAX_SUMMARY_CHARS)
}

/// Hard character-count truncation for embed inputs (no ellipsis — the text is
/// embedded, not shown).
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}

/// Display truncation for summaries: cut on a word boundary and append an
/// ellipsis, keeping the total length within `max`.
fn snippet(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_string();
    }
    let budget = max.saturating_sub(1); // leave room for the ellipsis
    let mut truncated: String = s.chars().take(budget).collect();
    // Back off to the last whitespace for a clean cut, unless that would
    // discard more than half the snippet.
    if let Some(idx) = truncated.rfind(char::is_whitespace)
        && idx >= budget / 2
    {
        truncated.truncate(idx);
    }
    format!("{}…", truncated.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(name: &str, signature: Option<&str>) -> SourceChunk {
        SourceChunk {
            name: name.into(),
            kind: "section".into(),
            content: String::new(),
            signature: signature.map(Into::into),
            doc_comment: None,
            start_line: None,
            end_line: None,
            references: vec![],
        }
    }

    #[test]
    fn code_item_has_structure() {
        let children = vec![chunk("get_user", Some("pub fn get_user(id: Uuid)"))];
        assert!(has_code_structure(None, &children));
        assert!(has_code_structure(Some("Module docs."), &[]));
    }

    #[test]
    fn prose_item_has_no_structure() {
        // Document sections: named, no signature.
        let children = vec![chunk("Overview", None), chunk("Details", None)];
        assert!(!has_code_structure(None, &children));
        assert!(!has_code_structure(None, &[]));
        // Blank module doc / signature don't count.
        assert!(!has_code_structure(Some("   "), &[chunk("x", Some("  "))]));
    }

    #[test]
    fn prose_doc_text_uses_content() {
        let content = "# Widget Spec\n\nThe widget cools itself via fins.";
        assert_eq!(prose_doc_text(content), content);
    }

    #[test]
    fn prose_section_text_joins_heading_and_body() {
        let text = prose_section_text("Cooling", "Heat vents through the top fins.");
        assert_eq!(text, "Cooling\nHeat vents through the top fins.");
    }

    #[test]
    fn prose_section_text_empty_body_falls_back_to_heading() {
        assert_eq!(prose_section_text("Overview", "   "), "Overview");
    }

    #[test]
    fn prose_doc_summary_is_title_plus_lead_not_whole_doc() {
        let content = "# Widget Spec\nURL: https://x\n\nThe widget cools via fins.\n\n## Cooling\nMore detail here that should NOT appear in the file summary.";
        let s = prose_doc_summary(content);
        assert!(s.contains("Widget Spec"));
        assert!(s.contains("The widget cools via fins."));
        // The deeper section detail must not be dumped into the file summary.
        assert!(!s.contains("should NOT appear"), "got: {s}");
        // URL metadata isn't used as the lead.
        assert!(!s.contains("URL:"), "got: {s}");
    }

    #[test]
    fn prose_doc_summary_is_capped() {
        let content = format!("# Title\n\n{}", "word ".repeat(400));
        assert!(prose_doc_summary(&content).chars().count() <= MAX_SUMMARY_CHARS);
    }

    #[test]
    fn prose_section_summary_is_content_without_heading() {
        // Heading is shown separately in the result header, so it's omitted here.
        let s = prose_section_summary("Cooling", "Heat vents through the top fins.");
        assert_eq!(s, "Heat vents through the top fins.");
    }

    #[test]
    fn prose_section_summary_empty_body_falls_back_to_heading() {
        assert_eq!(prose_section_summary("Overview", "  "), "Overview");
    }

    #[test]
    fn prose_text_is_truncated() {
        let big = "x".repeat(MAX_PROSE_CHARS + 5_000);
        assert_eq!(prose_doc_text(&big).chars().count(), MAX_PROSE_CHARS);
        let sec = prose_section_text("H", &big);
        assert_eq!(sec.chars().count(), MAX_PROSE_CHARS);
    }
}
