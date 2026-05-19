use std::fmt::Write;

use owo_colors::{OwoColorize, Stream, Style};

use super::runner::{CaseResult, SuiteResult};

pub fn render_json(result: &SuiteResult) -> String {
    serde_json::to_string_pretty(result).unwrap_or_default()
}

pub fn render_table(result: &SuiteResult) -> String {
    let mut out = String::new();

    let header_style = Style::new().bold();
    let title = format!(
        "wdpkr eval — {} ({} cases)",
        result.suite_name, result.summary.total_cases
    );
    writeln!(
        out,
        "{}",
        title.if_supports_color(Stream::Stdout, |s| s.style(header_style))
    )
    .unwrap();
    writeln!(out).unwrap();

    let col_header = format!(
        "{:<28} {:>6} {:>6} {:>7} {:>5} {:>8}",
        "Case", "P@K", "R@K", "Comp.", "Files", "Time"
    );
    writeln!(
        out,
        "{}",
        col_header.if_supports_color(Stream::Stdout, |s| s.dimmed())
    )
    .unwrap();

    for case in &result.cases {
        render_case_row(&mut out, case);
    }

    writeln!(out).unwrap();
    writeln!(
        out,
        "{}",
        "Summary".if_supports_color(Stream::Stdout, |s| s.style(header_style))
    )
    .unwrap();

    let s = &result.summary;
    writeln!(out, "  Cases:       {}", s.total_cases).unwrap();

    if let Some(p) = s.mean_precision_at_k {
        let style = score_style(p as f32);
        let val = format!("{p:.2}");
        writeln!(
            out,
            "  Mean P@K:    {}",
            val.if_supports_color(Stream::Stdout, |v| v.style(style))
        )
        .unwrap();
    }

    if let Some(r) = s.mean_recall_at_k {
        let style = score_style(r as f32);
        let val = format!("{r:.2}");
        writeln!(
            out,
            "  Mean R@K:    {}",
            val.if_supports_color(Stream::Stdout, |v| v.style(style))
        )
        .unwrap();
    }

    let comp_style = compression_style(s.mean_compression_ratio);
    let comp_val = format!("{:.2}", s.mean_compression_ratio);
    writeln!(
        out,
        "  Mean Comp.:  {}",
        comp_val.if_supports_color(Stream::Stdout, |v| v.style(comp_style))
    )
    .unwrap();

    let median_style = compression_style(s.median_compression_ratio);
    let median_val = format!("{:.2}", s.median_compression_ratio);
    writeln!(
        out,
        "  Med. Comp.:  {}",
        median_val.if_supports_color(Stream::Stdout, |v| v.style(median_style))
    )
    .unwrap();

    writeln!(out, "  Total time:  {}ms", result.elapsed_ms).unwrap();

    out
}

fn render_case_row(out: &mut String, case: &CaseResult) {
    let label = case
        .label
        .as_deref()
        .unwrap_or_else(|| truncate_query(&case.query));

    let (p_str, r_str) = match &case.relevance {
        Some(rel) => {
            let ps = score_style(rel.precision_at_k as f32);
            let rs = score_style(rel.recall_at_k as f32);
            (
                format!(
                    "{}",
                    format!("{:.2}", rel.precision_at_k)
                        .if_supports_color(Stream::Stdout, |v| v.style(ps))
                ),
                format!(
                    "{}",
                    format!("{:.2}", rel.recall_at_k)
                        .if_supports_color(Stream::Stdout, |v| v.style(rs))
                ),
            )
        }
        None => {
            let dash = format!("{}", "—".if_supports_color(Stream::Stdout, |s| s.dimmed()));
            (dash.clone(), dash)
        }
    };

    let comp_style = compression_style(case.compression.ratio);
    let comp_str = format!(
        "{}",
        format!("{:.2}", case.compression.ratio)
            .if_supports_color(Stream::Stdout, |v| v.style(comp_style))
    );

    writeln!(
        out,
        "{:<28} {:>6} {:>6} {:>7} {:>5} {:>6}ms",
        truncate_label(label, 27),
        p_str,
        r_str,
        comp_str,
        case.files_returned,
        case.elapsed_ms,
    )
    .unwrap();
}

fn truncate_label(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max - 1])
    }
}

fn truncate_query(q: &str) -> &str {
    if q.len() <= 27 { q } else { &q[..27] }
}

fn score_style(score: f32) -> Style {
    if score >= 0.80 {
        Style::new().green()
    } else if score >= 0.50 {
        Style::new().yellow()
    } else {
        Style::new().red()
    }
}

fn compression_style(ratio: f64) -> Style {
    if ratio <= 0.15 {
        Style::new().green()
    } else if ratio <= 0.30 {
        Style::new().yellow()
    } else {
        Style::new().red()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::metrics::{CompressionMetrics, RelevanceMetrics};
    use crate::eval::runner::SuiteSummary;

    fn sample_result() -> SuiteResult {
        SuiteResult {
            suite_name: "test-suite".into(),
            cases: vec![
                CaseResult {
                    query: "search pipeline".into(),
                    label: Some("search-pipeline".into()),
                    compression: CompressionMetrics {
                        output_tokens: 80,
                        source_tokens: 1000,
                        ratio: 0.08,
                    },
                    relevance: Some(RelevanceMetrics {
                        precision_at_k: 0.60,
                        recall_at_k: 1.0,
                        found: vec!["src/search/mod.rs".into()],
                        missed: vec![],
                        k: 5,
                    }),
                    files_returned: 5,
                    elapsed_ms: 120,
                },
                CaseResult {
                    query: "broad overview".into(),
                    label: Some("compression-only".into()),
                    compression: CompressionMetrics {
                        output_tokens: 60,
                        source_tokens: 1000,
                        ratio: 0.06,
                    },
                    relevance: None,
                    files_returned: 10,
                    elapsed_ms: 150,
                },
            ],
            summary: SuiteSummary {
                total_cases: 2,
                mean_compression_ratio: 0.07,
                median_compression_ratio: 0.07,
                mean_precision_at_k: Some(0.60),
                mean_recall_at_k: Some(1.0),
            },
            elapsed_ms: 270,
        }
    }

    #[test]
    fn table_contains_case_labels() {
        let out = render_table(&sample_result());
        assert!(out.contains("search-pipeline"));
        assert!(out.contains("compression-only"));
    }

    #[test]
    fn table_contains_summary() {
        let out = render_table(&sample_result());
        assert!(out.contains("Summary"));
        assert!(out.contains("Cases:"));
        assert!(out.contains("Mean Comp.:"));
    }

    #[test]
    fn table_contains_dash_for_no_relevance() {
        let out = render_table(&sample_result());
        assert!(out.contains("—"));
    }

    #[test]
    fn json_output_parses() {
        let json = render_json(&sample_result());
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["suite_name"], "test-suite");
        assert_eq!(v["cases"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn truncate_label_short() {
        assert_eq!(truncate_label("short", 27), "short");
    }

    #[test]
    fn truncate_label_long() {
        let long = "a".repeat(30);
        let truncated = truncate_label(&long, 27);
        assert_eq!(truncated.chars().count(), 27);
        assert!(truncated.ends_with('…'));
    }
}
