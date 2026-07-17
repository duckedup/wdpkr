use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Serialize;

use crate::search::{SearchParams, SearchRun};

use super::metrics::{self, CompressionMetrics, RelevanceMetrics, SymbolRelevanceMetrics};
use super::{EvalCase, EvalSuite};

// ── Source reader trait ──────────────────────────────────────────────────

#[async_trait]
pub trait SourceReader: Send + Sync {
    async fn read_file(&self, path: &str) -> Result<String>;
}

pub struct FsSourceReader {
    root: PathBuf,
}

impl FsSourceReader {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

#[async_trait]
impl SourceReader for FsSourceReader {
    async fn read_file(&self, path: &str) -> Result<String> {
        let full = self.root.join(path);
        std::fs::read_to_string(&full).with_context(|| format!("reading {}", full.display()))
    }
}

// ── Result types ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct CaseResult {
    pub query: String,
    pub label: Option<String>,
    pub tags: Vec<String>,
    pub compression: CompressionMetrics,
    pub relevance: Option<RelevanceMetrics>,
    pub symbol_relevance: Option<SymbolRelevanceMetrics>,
    pub files_returned: usize,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SuiteResult {
    pub suite_name: String,
    pub cases: Vec<CaseResult>,
    pub summary: SuiteSummary,
    pub by_tag: Vec<TagStats>,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SuiteSummary {
    pub total_cases: usize,
    pub mean_compression_ratio: f64,
    pub median_compression_ratio: f64,
    pub mean_precision_at_k: Option<f64>,
    pub mean_recall_at_k: Option<f64>,
    pub mean_reciprocal_rank: Option<f64>,
    pub mean_symbol_recall: Option<f64>,
    pub mean_symbol_reciprocal_rank: Option<f64>,
}

/// Aggregated metrics for all cases carrying a given tag. A case with
/// multiple tags contributes to each.
#[derive(Debug, Clone, Serialize)]
pub struct TagStats {
    pub tag: String,
    pub cases: usize,
    pub mean_precision_at_k: Option<f64>,
    pub mean_recall_at_k: Option<f64>,
    pub mean_reciprocal_rank: Option<f64>,
}

// ── Runner ───────────────────────────────────────────────────────────────

pub struct EvalRunner {
    search: SearchRun,
    reader: Box<dyn SourceReader>,
}

impl EvalRunner {
    pub fn new(search: SearchRun, reader: Box<dyn SourceReader>) -> Self {
        Self { search, reader }
    }

    pub async fn run_suite(&self, suite: &EvalSuite) -> Result<SuiteResult> {
        let start = Instant::now();
        let mut case_results = Vec::with_capacity(suite.cases.len());

        for case in &suite.cases {
            case_results.push(self.run_case(case).await?);
        }

        let summary = compute_summary(&case_results);
        let by_tag = compute_by_tag(&case_results);
        Ok(SuiteResult {
            suite_name: suite.name.clone(),
            cases: case_results,
            summary,
            by_tag,
            elapsed_ms: start.elapsed().as_millis() as u64,
        })
    }

    async fn run_case(&self, case: &EvalCase) -> Result<CaseResult> {
        let start = Instant::now();

        let params = SearchParams {
            query: case.query.clone(),
            top_k: case.top_k,
            symbols_per_file: 3,
            no_symbols: false,
            scope: vec![],
            filters: vec![],
        };

        let report = self.search.run(&params).await?;

        let mut source_contents = Vec::new();
        for file in &report.results {
            match self.reader.read_file(&file.path).await {
                Ok(content) => source_contents.push((file.path.clone(), content)),
                Err(_) => continue,
            }
        }

        let compression = metrics::compression(&report, &source_contents);

        let relevance = if case.expected_files.is_empty() {
            None
        } else {
            let returned_paths: Vec<String> =
                report.results.iter().map(|f| f.path.clone()).collect();
            Some(metrics::relevance(
                &returned_paths,
                &case.expected_files,
                case.top_k,
            ))
        };

        let symbol_relevance = if case.expected_symbols.is_empty() {
            None
        } else {
            // Flatten symbols into rank order: files in result order, symbols
            // in per-file order.
            let returned_symbols: Vec<String> = report
                .results
                .iter()
                .flat_map(|f| f.symbols.iter().map(|s| s.name.clone()))
                .collect();
            Some(metrics::symbol_relevance(
                &returned_symbols,
                &case.expected_symbols,
            ))
        };

        Ok(CaseResult {
            query: case.query.clone(),
            label: case.label.clone(),
            tags: case.tags.clone(),
            compression,
            relevance,
            symbol_relevance,
            files_returned: report.results.len(),
            elapsed_ms: start.elapsed().as_millis() as u64,
        })
    }
}

fn compute_summary(cases: &[CaseResult]) -> SuiteSummary {
    let total_cases = cases.len();

    let ratios: Vec<f64> = cases.iter().map(|c| c.compression.ratio).collect();
    let mean_compression_ratio = if ratios.is_empty() {
        0.0
    } else {
        ratios.iter().sum::<f64>() / ratios.len() as f64
    };

    let median_compression_ratio = median(&ratios);

    let precisions: Vec<f64> = cases
        .iter()
        .filter_map(|c| c.relevance.as_ref().map(|r| r.precision_at_k))
        .collect();
    let mean_precision_at_k = if precisions.is_empty() {
        None
    } else {
        Some(precisions.iter().sum::<f64>() / precisions.len() as f64)
    };

    let recalls: Vec<f64> = cases
        .iter()
        .filter_map(|c| c.relevance.as_ref().map(|r| r.recall_at_k))
        .collect();
    let mean_recall_at_k = if recalls.is_empty() {
        None
    } else {
        Some(recalls.iter().sum::<f64>() / recalls.len() as f64)
    };

    let rrs: Vec<f64> = cases
        .iter()
        .filter_map(|c| c.relevance.as_ref().map(|r| r.reciprocal_rank))
        .collect();
    let mean_reciprocal_rank = mean(&rrs);

    let sym_recalls: Vec<f64> = cases
        .iter()
        .filter_map(|c| c.symbol_relevance.as_ref().map(|r| r.recall))
        .collect();
    let mean_symbol_recall = mean(&sym_recalls);

    let sym_rrs: Vec<f64> = cases
        .iter()
        .filter_map(|c| c.symbol_relevance.as_ref().map(|r| r.reciprocal_rank))
        .collect();
    let mean_symbol_reciprocal_rank = mean(&sym_rrs);

    SuiteSummary {
        total_cases,
        mean_compression_ratio,
        median_compression_ratio,
        mean_precision_at_k,
        mean_recall_at_k,
        mean_reciprocal_rank,
        mean_symbol_recall,
        mean_symbol_reciprocal_rank,
    }
}

fn mean(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        None
    } else {
        Some(values.iter().sum::<f64>() / values.len() as f64)
    }
}

/// Aggregate relevance metrics per tag. A case contributes to each of its tags.
fn compute_by_tag(cases: &[CaseResult]) -> Vec<TagStats> {
    use std::collections::BTreeMap;

    let mut tags: BTreeMap<String, Vec<&CaseResult>> = BTreeMap::new();
    for case in cases {
        for tag in &case.tags {
            tags.entry(tag.clone()).or_default().push(case);
        }
    }

    tags.into_iter()
        .map(|(tag, group)| {
            let ps: Vec<f64> = group
                .iter()
                .filter_map(|c| c.relevance.as_ref().map(|r| r.precision_at_k))
                .collect();
            let rs: Vec<f64> = group
                .iter()
                .filter_map(|c| c.relevance.as_ref().map(|r| r.recall_at_k))
                .collect();
            let mrrs: Vec<f64> = group
                .iter()
                .filter_map(|c| c.relevance.as_ref().map(|r| r.reciprocal_rank))
                .collect();
            TagStats {
                tag,
                cases: group.len(),
                mean_precision_at_k: mean(&ps),
                mean_recall_at_k: mean(&rs),
                mean_reciprocal_rank: mean(&mrrs),
            }
        })
        .collect()
}

fn median(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = sorted.len() / 2;
    if sorted.len().is_multiple_of(2) {
        (sorted[mid - 1] + sorted[mid]) / 2.0
    } else {
        sorted[mid]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::SearchRun;
    use crate::store::{ChunkKind, Namespace, VectorDocument, VectorStore};
    use crate::testing::mock_embed::MockEmbedder;
    use crate::testing::mock_store::MockVectorStore;

    struct MockSourceReader {
        files: Vec<(String, String)>,
    }

    #[async_trait]
    impl SourceReader for MockSourceReader {
        async fn read_file(&self, path: &str) -> Result<String> {
            self.files
                .iter()
                .find(|(p, _)| p == path)
                .map(|(_, c)| c.clone())
                .ok_or_else(|| anyhow::anyhow!("not found: {path}"))
        }
    }

    async fn setup() -> (SearchRun, MockSourceReader) {
        let store = MockVectorStore::new();
        store
            .create_namespace(&Namespace::from("test"), 3)
            .await
            .unwrap();

        let docs = vec![
            VectorDocument {
                id: "f-main".into(),
                vector: vec![1.0, 0.0, 0.0],
                summary: "Main module".into(),
                file_path: "src/main.rs".into(),
                chunk_kind: ChunkKind::File,
                symbol_name: None,
                symbol_kind: None,
                start_line: None,
                end_line: None,
                language: Some("rust".into()),
                content_hash: None,
                calls: None,
                called_by: None,
                last_used_at: None,
            },
            VectorDocument {
                id: "f-lib".into(),
                vector: vec![0.0, 1.0, 0.0],
                summary: "Library root".into(),
                file_path: "src/lib.rs".into(),
                chunk_kind: ChunkKind::File,
                symbol_name: None,
                symbol_kind: None,
                start_line: None,
                end_line: None,
                language: Some("rust".into()),
                content_hash: None,
                calls: None,
                called_by: None,
                last_used_at: None,
            },
        ];
        store.upsert(&Namespace::from("test"), &docs).await.unwrap();

        let mut embedder = MockEmbedder::new(3);
        embedder.set_override("what is main", vec![0.95, 0.05, 0.0]);
        embedder.set_override("what is lib", vec![0.05, 0.95, 0.0]);

        let search = SearchRun::new(Box::new(embedder), Box::new(store), Namespace::from("test"));

        let reader = MockSourceReader {
            files: vec![
                ("src/main.rs".into(), "fn main() {}\n".repeat(50)),
                ("src/lib.rs".into(), "pub mod foo;\n".repeat(50)),
            ],
        };

        (search, reader)
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn run_case_compression() {
        let (search, reader) = setup().await;
        let runner = EvalRunner::new(search, Box::new(reader));

        let case = EvalCase {
            query: "what is main".into(),
            expected_files: vec![],
            expected_symbols: vec![],
            top_k: 5,
            label: Some("main-test".into()),
            tags: vec![],
        };

        let result = runner.run_case(&case).await.unwrap();
        assert!(result.compression.ratio < 1.0);
        assert!(result.relevance.is_none());
        assert!(result.files_returned > 0);
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn run_case_relevance() {
        let (search, reader) = setup().await;
        let runner = EvalRunner::new(search, Box::new(reader));

        let case = EvalCase {
            query: "what is main".into(),
            expected_files: vec!["src/main.rs".into()],
            expected_symbols: vec![],
            top_k: 5,
            label: None,
            tags: vec![],
        };

        let result = runner.run_case(&case).await.unwrap();
        let rel = result.relevance.unwrap();
        assert_eq!(rel.recall_at_k, 1.0);
        assert!(rel.found.contains(&"src/main.rs".to_string()));
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn run_suite_summary() {
        let (search, reader) = setup().await;
        let runner = EvalRunner::new(search, Box::new(reader));

        let suite = EvalSuite {
            name: "test-suite".into(),
            description: "test".into(),
            cases: vec![
                EvalCase {
                    query: "what is main".into(),
                    expected_files: vec!["src/main.rs".into()],
                    expected_symbols: vec![],
                    top_k: 5,
                    label: Some("main".into()),
                    tags: vec!["nl".into()],
                },
                EvalCase {
                    query: "what is lib".into(),
                    expected_files: vec!["src/lib.rs".into()],
                    expected_symbols: vec![],
                    top_k: 5,
                    label: Some("lib".into()),
                    tags: vec!["nl".into()],
                },
            ],
        };

        let result = runner.run_suite(&suite).await.unwrap();
        assert_eq!(result.cases.len(), 2);
        assert_eq!(result.summary.total_cases, 2);
        assert!(result.summary.mean_recall_at_k.is_some());

        // Both cases share the "nl" tag → one tag group covering 2 cases.
        assert_eq!(result.by_tag.len(), 1);
        assert_eq!(result.by_tag[0].tag, "nl");
        assert_eq!(result.by_tag[0].cases, 2);
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn run_case_symbol_relevance() {
        let (search, reader) = setup().await;
        let runner = EvalRunner::new(search, Box::new(reader));

        // The mock store returns file-level docs only (no symbols), so an
        // expected symbol is graded as missed — exercising the symbol path.
        let case = EvalCase {
            query: "what is main".into(),
            expected_files: vec![],
            expected_symbols: vec!["nonexistent_symbol".into()],
            top_k: 5,
            label: None,
            tags: vec![],
        };

        let result = runner.run_case(&case).await.unwrap();
        let sym = result.symbol_relevance.expect("symbol relevance computed");
        assert_eq!(sym.recall, 0.0);
        assert_eq!(sym.missed, vec!["nonexistent_symbol"]);
    }

    #[test]
    fn median_odd() {
        assert_eq!(median(&[1.0, 3.0, 2.0]), 2.0);
    }

    #[test]
    fn median_even() {
        assert_eq!(median(&[1.0, 2.0, 3.0, 4.0]), 2.5);
    }

    #[test]
    fn median_empty() {
        assert_eq!(median(&[]), 0.0);
    }

    #[test]
    fn median_single() {
        assert_eq!(median(&[5.0]), 5.0);
    }
}
