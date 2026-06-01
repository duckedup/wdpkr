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
    /// 1 / (rank of the first expected file in the ranked results), or 0.0 if
    /// none of the expected files appear in the top-k. Rewards putting a
    /// relevant file near the top — recall@k alone can't see ranking.
    pub reciprocal_rank: f64,
    /// 1-based rank of the first expected file found within the top-k.
    pub first_hit_rank: Option<usize>,
    pub found: Vec<String>,
    pub missed: Vec<String>,
    pub k: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct SymbolRelevanceMetrics {
    /// Fraction of expected symbols found among the returned symbols.
    pub recall: f64,
    /// 1 / (rank of the first expected symbol in the flattened, ranked symbol
    /// list), or 0.0 if none were returned.
    pub reciprocal_rank: f64,
    /// 1-based rank of the first expected symbol in the flattened symbol list.
    pub first_hit_rank: Option<usize>,
    pub found: Vec<String>,
    pub missed: Vec<String>,
}

/// Grade expected symbol names against the flattened, rank-ordered list of
/// symbol names returned across all result files. Matching is by name.
pub fn symbol_relevance(
    returned_symbols: &[String],
    expected_symbols: &[String],
) -> SymbolRelevanceMetrics {
    let mut found = Vec::new();
    let mut missed = Vec::new();
    for expected in expected_symbols {
        if returned_symbols.contains(expected) {
            found.push(expected.clone());
        } else {
            missed.push(expected.clone());
        }
    }

    let recall = if expected_symbols.is_empty() {
        0.0
    } else {
        found.len() as f64 / expected_symbols.len() as f64
    };

    let first_hit_rank = returned_symbols
        .iter()
        .position(|s| expected_symbols.contains(s))
        .map(|i| i + 1);
    let reciprocal_rank = first_hit_rank.map_or(0.0, |r| 1.0 / r as f64);

    SymbolRelevanceMetrics {
        recall,
        reciprocal_rank,
        first_hit_rank,
        found,
        missed,
    }
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
    let json = render_json(report, false).unwrap_or_default();
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

    let first_hit_rank = top_k
        .iter()
        .position(|p| expected_paths.iter().any(|e| e == *p))
        .map(|i| i + 1);
    let reciprocal_rank = first_hit_rank.map_or(0.0, |r| 1.0 / r as f64);

    RelevanceMetrics {
        precision_at_k,
        recall_at_k,
        reciprocal_rank,
        first_hit_rank,
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
                summary: Some("A file summary".into()),
                source: None,
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
    fn reciprocal_rank_first_position() {
        let returned = vec!["a.rs".into(), "b.rs".into()];
        let expected = vec!["a.rs".into()];
        let m = relevance(&returned, &expected, 5);
        assert_eq!(m.first_hit_rank, Some(1));
        assert_eq!(m.reciprocal_rank, 1.0);
    }

    #[test]
    fn reciprocal_rank_third_position() {
        let returned = vec!["x.rs".into(), "y.rs".into(), "a.rs".into()];
        let expected = vec!["a.rs".into()];
        let m = relevance(&returned, &expected, 5);
        assert_eq!(m.first_hit_rank, Some(3));
        assert!((m.reciprocal_rank - 1.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn reciprocal_rank_zero_when_missed() {
        let returned = vec!["x.rs".into(), "y.rs".into()];
        let expected = vec!["a.rs".into()];
        let m = relevance(&returned, &expected, 5);
        assert_eq!(m.first_hit_rank, None);
        assert_eq!(m.reciprocal_rank, 0.0);
    }

    #[test]
    fn reciprocal_rank_respects_k_window() {
        // The expected file is at rank 3 but k=2 excludes it.
        let returned = vec!["x.rs".into(), "y.rs".into(), "a.rs".into()];
        let expected = vec!["a.rs".into()];
        let m = relevance(&returned, &expected, 2);
        assert_eq!(m.first_hit_rank, None);
        assert_eq!(m.reciprocal_rank, 0.0);
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
    fn symbol_relevance_found_at_top() {
        let returned = vec!["process_item".into(), "document_id".into()];
        let expected = vec!["process_item".into()];
        let m = symbol_relevance(&returned, &expected);
        assert_eq!(m.recall, 1.0);
        assert_eq!(m.first_hit_rank, Some(1));
        assert_eq!(m.reciprocal_rank, 1.0);
    }

    #[test]
    fn symbol_relevance_partial() {
        let returned = vec!["a".into(), "b".into()];
        let expected = vec!["b".into(), "c".into()];
        let m = symbol_relevance(&returned, &expected);
        assert_eq!(m.recall, 0.5);
        assert_eq!(m.first_hit_rank, Some(2));
        assert_eq!(m.missed, vec!["c"]);
    }

    #[test]
    fn symbol_relevance_none_found() {
        let returned = vec!["x".into()];
        let expected = vec!["y".into()];
        let m = symbol_relevance(&returned, &expected);
        assert_eq!(m.recall, 0.0);
        assert_eq!(m.reciprocal_rank, 0.0);
        assert_eq!(m.first_hit_rank, None);
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
                summary: Some("Module summary".into()),
                source: None,
                symbols: vec![SymbolResult {
                    name: "foo".into(),
                    kind: "function".into(),
                    lines: [1, 20],
                    summary: Some("Does foo things".into()),
                    score: 0.85,
                    calls: None,
                    called_by: None,
                }],
            }],
        };
        let source = vec![("a.rs".into(), "fn foo() {}\n".repeat(100))];
        let m = compression(&report, &source);
        assert!(m.ratio < 1.0);
    }
}
