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
        "{:<28} {:>6} {:>6} {:>6} {:>6} {:>7} {:>5} {:>8}",
        "Case", "P@K", "R@K", "MRR", "SymR", "Comp.", "Files", "Time"
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

    if let Some(m) = s.mean_reciprocal_rank {
        let style = score_style(m as f32);
        let val = format!("{m:.2}");
        writeln!(
            out,
            "  Mean MRR:    {}",
            val.if_supports_color(Stream::Stdout, |v| v.style(style))
        )
        .unwrap();
    }

    if let Some(sr) = s.mean_symbol_recall {
        let style = score_style(sr as f32);
        let val = format!("{sr:.2}");
        writeln!(
            out,
            "  Mean Sym R:  {}",
            val.if_supports_color(Stream::Stdout, |v| v.style(style))
        )
        .unwrap();
    }

    if let Some(sm) = s.mean_symbol_reciprocal_rank {
        let style = score_style(sm as f32);
        let val = format!("{sm:.2}");
        writeln!(
            out,
            "  Mean Sym MRR:{}",
            format!(" {val}").if_supports_color(Stream::Stdout, |v| v.style(style))
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

    if !result.by_tag.is_empty() {
        writeln!(out).unwrap();
        writeln!(
            out,
            "{}",
            "By tag".if_supports_color(Stream::Stdout, |s| s.style(header_style))
        )
        .unwrap();
        let tag_header = format!(
            "{:<16} {:>3} {:>6} {:>6} {:>6}",
            "Tag", "n", "P@K", "R@K", "MRR"
        );
        writeln!(
            out,
            "{}",
            tag_header.if_supports_color(Stream::Stdout, |s| s.dimmed())
        )
        .unwrap();
        for t in &result.by_tag {
            writeln!(
                out,
                "{:<16} {:>3} {:>6} {:>6} {:>6}",
                t.tag,
                t.cases,
                fmt_opt(t.mean_precision_at_k),
                fmt_opt(t.mean_recall_at_k),
                fmt_opt(t.mean_reciprocal_rank),
            )
            .unwrap();
        }
    }

    out
}

fn fmt_opt(v: Option<f64>) -> String {
    match v {
        Some(x) => {
            let style = score_style(x as f32);
            format!(
                "{}",
                format!("{x:.2}").if_supports_color(Stream::Stdout, |s| s.style(style))
            )
        }
        None => "—".to_string(),
    }
}

fn render_case_row(out: &mut String, case: &CaseResult) {
    let label = case
        .label
        .as_deref()
        .unwrap_or_else(|| truncate_query(&case.query));

    let (p_str, r_str, mrr_str) = match &case.relevance {
        Some(rel) => {
            let ps = score_style(rel.precision_at_k as f32);
            let rs = score_style(rel.recall_at_k as f32);
            let ms = score_style(rel.reciprocal_rank as f32);
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
                format!(
                    "{}",
                    format!("{:.2}", rel.reciprocal_rank)
                        .if_supports_color(Stream::Stdout, |v| v.style(ms))
                ),
            )
        }
        None => {
            let dash = format!("{}", "—".if_supports_color(Stream::Stdout, |s| s.dimmed()));
            (dash.clone(), dash.clone(), dash)
        }
    };

    let sym_str = match &case.symbol_relevance {
        Some(sym) => {
            let ss = score_style(sym.recall as f32);
            format!(
                "{}",
                format!("{:.2}", sym.recall).if_supports_color(Stream::Stdout, |v| v.style(ss))
            )
        }
        None => format!("{}", "—".if_supports_color(Stream::Stdout, |s| s.dimmed())),
    };

    let comp_style = compression_style(case.compression.ratio);
    let comp_str = format!(
        "{}",
        format!("{:.2}", case.compression.ratio)
            .if_supports_color(Stream::Stdout, |v| v.style(comp_style))
    );

    writeln!(
        out,
        "{:<28} {:>6} {:>6} {:>6} {:>6} {:>7} {:>5} {:>6}ms",
        truncate_label(label, 27),
        p_str,
        r_str,
        mrr_str,
        sym_str,
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
    use crate::eval::metrics::{CompressionMetrics, RelevanceMetrics, SymbolRelevanceMetrics};
    use crate::eval::runner::{SuiteSummary, TagStats};

    fn sample_result() -> SuiteResult {
        SuiteResult {
            suite_name: "test-suite".into(),
            cases: vec![
                CaseResult {
                    query: "search pipeline".into(),
                    label: Some("search-pipeline".into()),
                    tags: vec!["search".into()],
                    compression: CompressionMetrics {
                        output_tokens: 80,
                        source_tokens: 1000,
                        ratio: 0.08,
                    },
                    relevance: Some(RelevanceMetrics {
                        precision_at_k: 0.60,
                        recall_at_k: 1.0,
                        reciprocal_rank: 1.0,
                        first_hit_rank: Some(1),
                        found: vec!["src/search/mod.rs".into()],
                        missed: vec![],
                        k: 5,
                    }),
                    symbol_relevance: Some(SymbolRelevanceMetrics {
                        recall: 1.0,
                        reciprocal_rank: 0.5,
                        first_hit_rank: Some(2),
                        found: vec!["run".into()],
                        missed: vec![],
                    }),
                    files_returned: 5,
                    elapsed_ms: 120,
                },
                CaseResult {
                    query: "broad overview".into(),
                    label: Some("compression-only".into()),
                    tags: vec![],
                    compression: CompressionMetrics {
                        output_tokens: 60,
                        source_tokens: 1000,
                        ratio: 0.06,
                    },
                    relevance: None,
                    symbol_relevance: None,
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
                mean_reciprocal_rank: Some(1.0),
                mean_symbol_recall: Some(1.0),
                mean_symbol_reciprocal_rank: Some(0.5),
            },
            by_tag: vec![TagStats {
                tag: "search".into(),
                cases: 1,
                mean_precision_at_k: Some(0.60),
                mean_recall_at_k: Some(1.0),
                mean_reciprocal_rank: Some(1.0),
            }],
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
    fn table_contains_by_tag_section() {
        let out = render_table(&sample_result());
        assert!(out.contains("By tag"));
        assert!(out.contains("search"));
    }

    #[test]
    fn table_contains_symbol_summary() {
        let out = render_table(&sample_result());
        assert!(out.contains("Mean Sym R"));
        assert!(out.contains("SymR"));
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
