use serde::Serialize;

use crate::search::SearchReport;
use crate::search::output::render_json;

#[derive(Debug, Clone, Serialize)]
pub struct CompressionMetrics {
    pub output_tokens: usize,
    pub source_tokens: usize,
    pub ratio: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct RelevanceMetrics {
    pub precision_at_k: f64,
    pub recall_at_k: f64,
    pub found: Vec<String>,
    pub missed: Vec<String>,
    pub k: usize,
}

pub fn approx_token_count(text: &str) -> usize {
    let count = text.chars().count() / 4;
    if count == 0 && !text.is_empty() {
        1
    } else {
        count
    }
}

pub fn compression(
    report: &SearchReport,
    source_contents: &[(String, String)],
) -> CompressionMetrics {
    let json = render_json(report).unwrap_or_default();
    let output_tokens = approx_token_count(&json);

    let source_tokens: usize = source_contents
        .iter()
        .map(|(_, content)| approx_token_count(content))
        .sum();

    let ratio = if source_tokens == 0 {
        0.0
    } else {
        output_tokens as f64 / source_tokens as f64
    };

    CompressionMetrics {
        output_tokens,
        source_tokens,
        ratio,
    }
}

pub fn relevance(
    returned_paths: &[String],
    expected_paths: &[String],
    k: usize,
) -> RelevanceMetrics {
    let top_k: Vec<&String> = returned_paths.iter().take(k).collect();

    let mut found = Vec::new();
    let mut missed = Vec::new();

    for expected in expected_paths {
        if top_k.contains(&expected) {
            found.push(expected.clone());
        } else {
            missed.push(expected.clone());
        }
    }

    let precision_at_k = if top_k.is_empty() {
        0.0
    } else {
        found.len() as f64 / top_k.len() as f64
    };

    let recall_at_k = if expected_paths.is_empty() {
        0.0
    } else {
        found.len() as f64 / expected_paths.len() as f64
    };

    RelevanceMetrics {
        precision_at_k,
        recall_at_k,
        found,
        missed,
        k,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::{FileResult, SymbolResult};

    #[test]
    fn token_count_empty() {
        assert_eq!(approx_token_count(""), 0);
    }

    #[test]
    fn token_count_short() {
        assert_eq!(approx_token_count("hi"), 1);
    }

    #[test]
    fn token_count_longer() {
        let text = "a".repeat(400);
        assert_eq!(approx_token_count(&text), 100);
    }

    #[test]
    fn compression_ratio_basic() {
        let report = SearchReport {
            query: "test".into(),
            namespace: "ns".into(),
            indexed_at: None,
            results: vec![FileResult {
                path: "a.rs".into(),
                score: 0.9,
                summary: "A file summary".into(),
                symbols: vec![],
            }],
        };
        let source = vec![("a.rs".into(), "x".repeat(4000))];
        let m = compression(&report, &source);
        assert!(m.ratio < 1.0);
        assert!(m.source_tokens > m.output_tokens);
    }

    #[test]
    fn compression_no_source_files() {
        let report = SearchReport {
            query: "test".into(),
            namespace: "ns".into(),
            indexed_at: None,
            results: vec![],
        };
        let m = compression(&report, &[]);
        assert_eq!(m.ratio, 0.0);
    }

    #[test]
    fn relevance_perfect_recall() {
        let returned = vec!["a.rs".into(), "b.rs".into(), "c.rs".into()];
        let expected = vec!["a.rs".into(), "b.rs".into()];
        let m = relevance(&returned, &expected, 3);
        assert_eq!(m.recall_at_k, 1.0);
        assert!((m.precision_at_k - 2.0 / 3.0).abs() < 0.01);
        assert!(m.missed.is_empty());
    }

    #[test]
    fn relevance_partial_recall() {
        let returned = vec!["a.rs".into(), "c.rs".into()];
        let expected = vec!["a.rs".into(), "b.rs".into()];
        let m = relevance(&returned, &expected, 2);
        assert_eq!(m.recall_at_k, 0.5);
        assert_eq!(m.precision_at_k, 0.5);
        assert_eq!(m.missed, vec!["b.rs"]);
    }

    #[test]
    fn relevance_no_results() {
        let m = relevance(&[], &["a.rs".into()], 5);
        assert_eq!(m.precision_at_k, 0.0);
        assert_eq!(m.recall_at_k, 0.0);
    }

    #[test]
    fn relevance_no_expected() {
        let returned = vec!["a.rs".into()];
        let m = relevance(&returned, &[], 5);
        assert_eq!(m.recall_at_k, 0.0);
    }

    #[test]
    fn relevance_k_limits_returned() {
        let returned = vec!["a.rs".into(), "b.rs".into(), "c.rs".into()];
        let expected = vec!["c.rs".into()];
        let m = relevance(&returned, &expected, 2);
        assert_eq!(m.recall_at_k, 0.0);
        assert_eq!(m.missed, vec!["c.rs"]);
    }

    #[test]
    fn compression_with_symbols() {
        let report = SearchReport {
            query: "test".into(),
            namespace: "ns".into(),
            indexed_at: None,
            results: vec![FileResult {
                path: "a.rs".into(),
                score: 0.9,
                summary: "Module summary".into(),
                symbols: vec![SymbolResult {
                    name: "foo".into(),
                    kind: "function".into(),
                    lines: [1, 20],
                    summary: "Does foo things".into(),
                    score: 0.85,
                }],
            }],
        };
        let source = vec![("a.rs".into(), "fn foo() {}\n".repeat(100))];
        let m = compression(&report, &source);
        assert!(m.ratio < 1.0);
    }
}
