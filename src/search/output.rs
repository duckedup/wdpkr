//! Search result rendering: JSON (default, for agents) and `--pretty`
//! (human-readable with ANSI colors).

use std::fmt::Write;

use anyhow::Result;

use super::{FileResult, SearchReport};

/// Render the report as pretty-printed JSON. This is the default output
/// format — agents parse it directly.
pub fn render_json(report: &SearchReport) -> Result<String> {
    Ok(serde_json::to_string_pretty(report)?)
}

/// Render the report as human-readable ANSI-colored text for `--pretty`.
pub fn render_pretty(report: &SearchReport) -> String {
    let mut out = String::new();

    if report.results.is_empty() {
        writeln!(out, "{DIM}No results.{RESET}").unwrap();
        return out;
    }

    if let Some(ref sha) = report.indexed_at {
        writeln!(out, "{DIM}indexed at {sha}{RESET}").unwrap();
    }

    for (i, file) in report.results.iter().enumerate() {
        if i > 0 {
            writeln!(out).unwrap();
        }
        render_file(&mut out, file);
    }
    out
}

fn render_file(out: &mut String, file: &FileResult) {
    writeln!(
        out,
        "{BOLD}{CYAN}{path}{RESET} {DIM}({score:.2}){RESET}",
        path = file.path,
        score = file.score,
    )
    .unwrap();
    writeln!(out, "  {}", file.summary).unwrap();

    let count = file.symbols.len();
    for (i, sym) in file.symbols.iter().enumerate() {
        let is_last = i == count - 1;
        let branch = if is_last { "└─" } else { "├─" };
        let cont = if is_last { "   " } else { "│  " };

        writeln!(
            out,
            "  {DIM}{branch}{RESET} {BOLD}{name}{RESET} {DIM}({kind}, L{start}-{end}) — {score:.2}{RESET}",
            name = sym.name,
            kind = sym.kind,
            start = sym.lines[0],
            end = sym.lines[1],
            score = sym.score,
        )
        .unwrap();
        writeln!(out, "  {cont}{}", sym.summary).unwrap();
    }
}

// ANSI escape sequences — kept as constants rather than pulling in a crate.
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const CYAN: &str = "\x1b[36m";
const RESET: &str = "\x1b[0m";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::SymbolResult;

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
        let json = render_json(&report).unwrap();
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
        let json = render_json(&report).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["results"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn pretty_contains_file_paths() {
        let report = sample_report();
        let out = render_pretty(&report);
        assert!(out.contains("src/finance/commission.rs"));
        assert!(out.contains("src/auth/login.rs"));
    }

    #[test]
    fn pretty_contains_symbol_names() {
        let report = sample_report();
        let out = render_pretty(&report);
        assert!(out.contains("release_payment"));
        assert!(out.contains("correct_amount"));
        assert!(out.contains("authenticate"));
    }

    #[test]
    fn pretty_contains_scores() {
        let report = sample_report();
        let out = render_pretty(&report);
        assert!(out.contains("0.87"));
        assert!(out.contains("0.91"));
    }

    #[test]
    fn pretty_uses_tree_characters() {
        let report = sample_report();
        let out = render_pretty(&report);
        // Commission file has 2 symbols: first gets ├─, last gets └─
        assert!(out.contains("├─"));
        assert!(out.contains("└─"));
    }

    #[test]
    fn pretty_contains_indexed_at() {
        let report = sample_report();
        let out = render_pretty(&report);
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
        let out = render_pretty(&report);
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
        let out = render_pretty(&report);
        assert!(out.contains("└─"));
        assert!(!out.contains("├─"));
    }
}
