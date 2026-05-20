//! Search result rendering: JSON (default, for agents) and `--pretty`
//! (human-readable with ANSI colors).

use std::fmt::Write;

use anyhow::Result;
use owo_colors::{OwoColorize, Stream, Style};

use super::{FileResult, SearchReport, SymbolResult};

/// Render the report as pretty-printed JSON. This is the default output
/// format — agents parse it directly. When `terse`, summaries are
/// truncated to the first sentence and symbols are omitted.
pub fn render_json(report: &SearchReport, terse: bool) -> Result<String> {
    if !terse {
        return Ok(serde_json::to_string_pretty(report)?);
    }
    let terse_report = SearchReport {
        query: report.query.clone(),
        namespace: report.namespace.clone(),
        indexed_at: report.indexed_at.clone(),
        results: report
            .results
            .iter()
            .map(|f| FileResult {
                path: f.path.clone(),
                score: f.score,
                summary: first_sentence(&f.summary).to_string(),
                symbols: vec![],
            })
            .collect(),
    };
    Ok(serde_json::to_string_pretty(&terse_report)?)
}

/// Render the report as human-readable ANSI-colored text for `--pretty`.
/// When `terse`, each file is one line: `path (score) — summary`.
pub fn render_pretty(report: &SearchReport, terse: bool) -> String {
    let mut out = String::new();

    if report.results.is_empty() {
        writeln!(
            out,
            "{}",
            "No results.".if_supports_color(Stream::Stdout, |s| s.dimmed())
        )
        .unwrap();
        return out;
    }

    if !terse && let Some(ref sha) = report.indexed_at {
        writeln!(
            out,
            "{}",
            format!("indexed at {sha}").if_supports_color(Stream::Stdout, |s| s.dimmed())
        )
        .unwrap();
    }

    for (i, file) in report.results.iter().enumerate() {
        if terse {
            render_file_terse(&mut out, file);
        } else {
            if i > 0 {
                writeln!(out).unwrap();
            }
            render_file(&mut out, file);
        }
    }
    out
}

fn first_sentence(s: &str) -> &str {
    s.find(". ")
        .or_else(|| s.find(".\n"))
        .map(|i| &s[..=i])
        .unwrap_or(s)
}

fn render_file_terse(out: &mut String, file: &FileResult) {
    let path_style = Style::new().cyan().bold();
    let path = file
        .path
        .if_supports_color(Stream::Stdout, |s| s.style(path_style));
    let ss = score_style(file.score);
    let score_text = format!("({:.2})", file.score);
    let score_display = score_text.if_supports_color(Stream::Stdout, |s| s.style(ss));
    let summary = first_sentence(&file.summary);
    let summary_display = summary.if_supports_color(Stream::Stdout, |s| s.dimmed());
    writeln!(out, "{path} {score_display} — {summary_display}").unwrap();
}

fn score_style(score: f32) -> Style {
    if score >= 0.80 {
        Style::new().green()
    } else if score >= 0.50 {
        Style::new().yellow()
    } else {
        Style::new().dimmed()
    }
}

fn kind_style(kind: &str) -> Style {
    match kind {
        "function" | "method" => Style::new(),
        "struct" | "class" => Style::new().cyan(),
        "trait" | "interface" => Style::new().magenta(),
        "enum" => Style::new().yellow(),
        "impl" => Style::new().cyan(),
        _ => Style::new().dimmed(),
    }
}

fn render_file(out: &mut String, file: &FileResult) {
    let path_style = Style::new().cyan().bold();
    let path = file
        .path
        .if_supports_color(Stream::Stdout, |s| s.style(path_style));
    let ss = score_style(file.score);
    let score_text = format!("({:.2})", file.score);
    let score_display = score_text.if_supports_color(Stream::Stdout, |s| s.style(ss));
    writeln!(out, "{path} {score_display}",).unwrap();
    writeln!(
        out,
        "  {}",
        file.summary
            .if_supports_color(Stream::Stdout, |s| s.dimmed())
    )
    .unwrap();

    let count = file.symbols.len();
    for (i, sym) in file.symbols.iter().enumerate() {
        let is_last = i == count - 1;
        render_symbol(out, sym, is_last);
    }
}

fn render_symbol(out: &mut String, sym: &SymbolResult, is_last: bool) {
    let branch = if is_last { "└─" } else { "├─" };
    let cont = if is_last { "   " } else { "│  " };

    let branch_display = branch.if_supports_color(Stream::Stdout, |s| s.dimmed());
    let name_display = sym.name.if_supports_color(Stream::Stdout, |s| s.bold());

    let ks = kind_style(&sym.kind);
    let kind_display = sym.kind.if_supports_color(Stream::Stdout, |s| s.style(ks));
    let line_range = format!("L{}-{}", sym.lines[0], sym.lines[1]);
    let line_display = line_range.if_supports_color(Stream::Stdout, |s| s.dimmed());

    let ss = score_style(sym.score);
    let score_text = format!("— {:.2}", sym.score);
    let score_display = score_text.if_supports_color(Stream::Stdout, |s| s.style(ss));

    writeln!(
        out,
        "  {branch_display} {name_display} ({kind_display}, {line_display}) {score_display}",
    )
    .unwrap();
    writeln!(out, "  {cont}{}", sym.summary).unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_report() -> SearchReport {
        SearchReport {
            query: "release commission payments".into(),
            namespace: "test-repo".into(),
            indexed_at: Some("abc123".into()),
            results: vec![
                FileResult {
                    path: "src/finance/commission.rs".into(),
                    score: 0.87,
                    summary: "Commission payment release service".into(),
                    symbols: vec![
                        SymbolResult {
                            name: "release_payment".into(),
                            kind: "function".into(),
                            lines: [42, 78],
                            summary: "Releases commission for a payee".into(),
                            score: 0.91,
                        },
                        SymbolResult {
                            name: "correct_amount".into(),
                            kind: "function".into(),
                            lines: [80, 95],
                            summary: "Corrects commission amount".into(),
                            score: 0.83,
                        },
                    ],
                },
                FileResult {
                    path: "src/auth/login.rs".into(),
                    score: 0.72,
                    summary: "Authentication and session management".into(),
                    symbols: vec![SymbolResult {
                        name: "authenticate".into(),
                        kind: "function".into(),
                        lines: [10, 30],
                        summary: "Authenticates a user".into(),
                        score: 0.68,
                    }],
                },
            ],
        }
    }

    #[test]
    fn json_output_has_spec_fields() {
        let report = sample_report();
        let json = render_json(&report, false).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["query"], "release commission payments");
        assert_eq!(v["namespace"], "test-repo");
        assert_eq!(v["indexed_at"], "abc123");
        assert_eq!(v["results"][0]["path"], "src/finance/commission.rs");
        assert_eq!(v["results"][0]["symbols"][0]["name"], "release_payment");
        assert_eq!(v["results"][0]["symbols"][0]["lines"][0], 42);
    }

    #[test]
    fn json_round_trips_cleanly() {
        let report = sample_report();
        let json = render_json(&report, false).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["results"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn pretty_contains_file_paths() {
        let report = sample_report();
        let out = render_pretty(&report, false);
        assert!(out.contains("src/finance/commission.rs"));
        assert!(out.contains("src/auth/login.rs"));
    }

    #[test]
    fn pretty_contains_symbol_names() {
        let report = sample_report();
        let out = render_pretty(&report, false);
        assert!(out.contains("release_payment"));
        assert!(out.contains("correct_amount"));
        assert!(out.contains("authenticate"));
    }

    #[test]
    fn pretty_contains_scores() {
        let report = sample_report();
        let out = render_pretty(&report, false);
        assert!(out.contains("0.87"));
        assert!(out.contains("0.91"));
    }

    #[test]
    fn pretty_uses_tree_characters() {
        let report = sample_report();
        let out = render_pretty(&report, false);
        assert!(out.contains("├─"));
        assert!(out.contains("└─"));
    }

    #[test]
    fn pretty_contains_indexed_at() {
        let report = sample_report();
        let out = render_pretty(&report, false);
        assert!(out.contains("abc123"));
    }

    #[test]
    fn pretty_handles_empty_results() {
        let report = SearchReport {
            query: "nothing".into(),
            namespace: "test".into(),
            indexed_at: None,
            results: vec![],
        };
        let out = render_pretty(&report, false);
        assert!(out.contains("No results"));
    }

    #[test]
    fn pretty_single_symbol_uses_last_branch() {
        let report = SearchReport {
            query: "test".into(),
            namespace: "test".into(),
            indexed_at: None,
            results: vec![FileResult {
                path: "a.rs".into(),
                score: 0.5,
                summary: "A file".into(),
                symbols: vec![SymbolResult {
                    name: "only_one".into(),
                    kind: "function".into(),
                    lines: [1, 10],
                    summary: "The only symbol".into(),
                    score: 0.4,
                }],
            }],
        };
        let out = render_pretty(&report, false);
        assert!(out.contains("└─"));
        assert!(!out.contains("├─"));
    }

    #[test]
    fn first_sentence_splits_on_period_space() {
        assert_eq!(
            first_sentence("Hello world. More text here."),
            "Hello world."
        );
    }

    #[test]
    fn first_sentence_no_period_returns_all() {
        assert_eq!(first_sentence("No period here"), "No period here");
    }

    #[test]
    fn first_sentence_period_at_end_returns_all() {
        assert_eq!(first_sentence("Ends with period."), "Ends with period.");
    }

    #[test]
    fn first_sentence_empty_string() {
        assert_eq!(first_sentence(""), "");
    }

    #[test]
    fn terse_pretty_one_line_per_file() {
        let report = sample_report();
        let out = render_pretty(&report, true);
        // Should not contain tree characters (no symbols)
        assert!(!out.contains("├─"));
        assert!(!out.contains("└─"));
        // Should contain file paths
        assert!(out.contains("src/finance/commission.rs"));
        assert!(out.contains("src/auth/login.rs"));
        // Should contain the em-dash separator
        assert!(out.contains("—"));
        // Should NOT contain "indexed at" header
        assert!(!out.contains("indexed at"));
    }

    #[test]
    fn terse_json_truncates_summaries() {
        let report = SearchReport {
            query: "test".into(),
            namespace: "test".into(),
            indexed_at: None,
            results: vec![FileResult {
                path: "a.rs".into(),
                score: 0.9,
                summary: "First sentence. Second sentence. Third.".into(),
                symbols: vec![SymbolResult {
                    name: "func".into(),
                    kind: "function".into(),
                    lines: [1, 10],
                    summary: "A function".into(),
                    score: 0.8,
                }],
            }],
        };
        let json_str = render_json(&report, true).unwrap();
        let json: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        let summary = json["results"][0]["summary"].as_str().unwrap();
        assert_eq!(summary, "First sentence.");
        // Symbols should be empty in terse mode
        assert!(json["results"][0]["symbols"].as_array().unwrap().is_empty());
    }
}
