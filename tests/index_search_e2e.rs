//! End-to-end integration tests: index a fixture repo, then search it.
//!
//! Uses real tree-sitter chunking against real source files in a temp git
//! repo. Summarizer and embedder are mocks — no API calls, no cost.
//! The mock store is shared between index and search so documents flow
//! through the full pipeline: file → chunk → summarize → embed → upsert
//! → search → results.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use wdpkr::chunk::tree_sitter::TreeSitterChunker;
use wdpkr::indexer::IndexRun;
use wdpkr::search::{SearchParams, SearchRun};
use wdpkr::store::{Namespace, VectorStore};
use wdpkr::testing::mock_embed::MockEmbedder;
use wdpkr::testing::mock_store::MockVectorStore;
use wdpkr::testing::mock_summarize::MockSummarizer;

// ── Fixture helpers ───────────────────────────────────────────────────────

fn unique_tempdir(label: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path =
        std::env::temp_dir().join(format!("wdpkr-e2e-{label}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&path).unwrap();
    path
}

/// Create a temp directory with a git repo containing fixture source files.
fn create_fixture_repo(label: &str) -> PathBuf {
    let dir = unique_tempdir(label);

    git(&dir, &["init"]);
    git(&dir, &["config", "user.email", "test@wdpkr.dev"]);
    git(&dir, &["config", "user.name", "Test"]);

    // Rust file with two functions
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("payments.rs"),
        r#"use std::io;

/// Releases a commission payment to a payee.
pub fn release_payment(payee_id: u64, amount: f64) -> Result<(), String> {
    if amount <= 0.0 {
        return Err("amount must be positive".into());
    }
    println!("Releasing {amount} to payee {payee_id}");
    Ok(())
}

/// Processes a refund for a previously released payment.
pub fn process_refund(payment_id: u64) -> Result<(), String> {
    println!("Refunding payment {payment_id}");
    Ok(())
}
"#,
    )
    .unwrap();

    // Python file
    std::fs::write(
        src.join("auth.py"),
        r#"
def authenticate(username: str, password: str) -> bool:
    """Authenticate a user against the credential store."""
    return username == "admin" and password == "secret"

def revoke_session(session_id: str) -> None:
    """Revoke an active session."""
    print(f"Revoking session {session_id}")
"#,
    )
    .unwrap();

    // Go file
    std::fs::write(
        src.join("handler.go"),
        r#"package api

import "net/http"

// HandleRequest processes an incoming HTTP request.
func HandleRequest(w http.ResponseWriter, r *http.Request) {
    w.WriteHeader(http.StatusOK)
}
"#,
    )
    .unwrap();

    // Non-code file (should get file-level only, no symbols)
    std::fs::write(
        dir.join("README.md"),
        "# Test Repo\n\nA fixture for integration tests.\n",
    )
    .unwrap();

    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-m", "initial commit"]);

    dir
}

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {} failed in {}: {}",
        args.join(" "),
        dir.display(),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn cleanup(dir: &Path) {
    std::fs::remove_dir_all(dir).ok();
}

fn build_index_run(store: MockVectorStore, embedder: MockEmbedder) -> IndexRun {
    IndexRun::new(
        Arc::new(TreeSitterChunker::new()),
        Arc::new(MockSummarizer::new()),
        Arc::new(embedder),
        Arc::new(store),
        Namespace::from("test-e2e"),
        1,
    )
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn full_index_populates_store() {
    let dir = create_fixture_repo("full-index");
    let store = MockVectorStore::new();
    let embedder = MockEmbedder::new(8);
    let index = build_index_run(store, embedder);

    let report = index.run(true, &dir).await.unwrap();

    assert!(report.files_processed > 0, "should process files");
    assert_eq!(report.files_failed, 0, "no files should fail");
    assert!(report.vectors_upserted > 0, "should upsert vectors");
    assert!(report.hwm_advanced_to.is_some(), "should set HWM");

    cleanup(&dir);
}

#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn index_produces_file_and_symbol_documents() {
    let dir = create_fixture_repo("docs");
    let store = MockVectorStore::new();
    let embedder = MockEmbedder::new(8);
    let index = build_index_run(store, embedder);

    let report = index.run(true, &dir).await.unwrap();

    // We have 4 files: payments.rs (2 fns), auth.py (2 fns), handler.go (1 fn), README.md
    // Expected: >= 4 file-level + several symbol-level
    assert!(
        report.vectors_upserted >= 4,
        "expected at least 4 docs (one per file), got {}",
        report.vectors_upserted
    );

    cleanup(&dir);
}

#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn hwm_is_stored_after_indexing() {
    let dir = create_fixture_repo("hwm");
    let store = MockVectorStore::new();
    let embedder = MockEmbedder::new(8);
    let index = build_index_run(store, embedder);

    let report = index.run(true, &dir).await.unwrap();

    // The IndexRun owns the store now, but we can check via the report
    assert!(report.hwm_advanced_to.is_some());
    let sha = report.hwm_advanced_to.unwrap();
    assert!(sha.len() >= 7);
    assert!(sha.chars().all(|c| c.is_ascii_hexdigit()));

    cleanup(&dir);
}

#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn index_then_search_round_trip() {
    let dir = create_fixture_repo("round-trip");

    // Shared store: index writes to it, search reads from it
    let store = Arc::new(MockVectorStore::new());
    let ns = Namespace::from("test-roundtrip");

    // Index
    let index = IndexRun::new(
        Arc::new(TreeSitterChunker::new()),
        Arc::new(MockSummarizer::new()),
        Arc::new(MockEmbedder::new(8)),
        Arc::new(ArcStore(store.clone())),
        ns.clone(),
        1,
    );
    let report = index.run(true, &dir).await.unwrap();
    assert!(report.vectors_upserted > 0);

    // Search
    let search = SearchRun::new(
        Box::new(MockEmbedder::new(8)),
        Box::new(ArcStore(store.clone())),
        ns.clone(),
    );
    let result = search
        .run(&SearchParams {
            query: "payment release".into(),
            top_k: 10,
            symbols_per_file: 5,
            no_symbols: false,
            scope: vec![],
            filters: vec![],
        })
        .await
        .unwrap();

    // Results should come back (we can't assert on ranking with hash-based
    // mock embeddings, but we can verify structural correctness)
    assert!(!result.results.is_empty(), "search should return results");
    assert_eq!(result.namespace, "test-roundtrip");
    assert!(result.indexed_at.is_some(), "should include HWM");

    // Every result should have a path and summary
    for file_result in &result.results {
        assert!(!file_result.path.is_empty());
        assert!(!file_result.summary.is_empty());
    }

    cleanup(&dir);
}

#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn search_finds_indexed_file_paths() {
    let dir = create_fixture_repo("paths");

    let store = Arc::new(MockVectorStore::new());
    let ns = Namespace::from("test-paths");

    let index = IndexRun::new(
        Arc::new(TreeSitterChunker::new()),
        Arc::new(MockSummarizer::new()),
        Arc::new(MockEmbedder::new(8)),
        Arc::new(ArcStore(store.clone())),
        ns.clone(),
        1,
    );
    index.run(true, &dir).await.unwrap();

    let search = SearchRun::new(
        Box::new(MockEmbedder::new(8)),
        Box::new(ArcStore(store.clone())),
        ns.clone(),
    );
    let result = search
        .run(&SearchParams {
            query: "anything".into(),
            top_k: 20,
            symbols_per_file: 10,
            no_symbols: false,
            scope: vec![],
            filters: vec![],
        })
        .await
        .unwrap();

    let paths: Vec<&str> = result.results.iter().map(|r| r.path.as_str()).collect();
    // payments.rs was indexed → it should appear somewhere in results
    let has_payments = paths.iter().any(|p| p.contains("payments.rs"));
    assert!(
        has_payments,
        "expected payments.rs in results; got: {paths:?}"
    );

    cleanup(&dir);
}

#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn symbols_appear_nested_under_files() {
    let dir = create_fixture_repo("symbols");

    let store = Arc::new(MockVectorStore::new());
    let ns = Namespace::from("test-symbols");

    let index = IndexRun::new(
        Arc::new(TreeSitterChunker::new()),
        Arc::new(MockSummarizer::new()),
        Arc::new(MockEmbedder::new(8)),
        Arc::new(ArcStore(store.clone())),
        ns.clone(),
        1,
    );
    index.run(true, &dir).await.unwrap();

    let search = SearchRun::new(
        Box::new(MockEmbedder::new(8)),
        Box::new(ArcStore(store.clone())),
        ns.clone(),
    );
    let result = search
        .run(&SearchParams {
            query: "anything".into(),
            top_k: 20,
            symbols_per_file: 10,
            no_symbols: false,
            scope: vec![],
            filters: vec![],
        })
        .await
        .unwrap();

    // Find the payments.rs result and check it has symbols
    let payments = result
        .results
        .iter()
        .find(|r| r.path.contains("payments.rs"));
    if let Some(p) = payments {
        assert!(
            !p.symbols.is_empty(),
            "payments.rs should have symbols (release_payment, process_refund)"
        );
        let sym_names: Vec<&str> = p.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(
            sym_names
                .iter()
                .any(|n| n.contains("release_payment") || n.contains("process_refund")),
            "expected release_payment or process_refund; got: {sym_names:?}"
        );
    }

    cleanup(&dir);
}

#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn scope_filter_limits_results_after_index() {
    let dir = create_fixture_repo("scope");

    let store = Arc::new(MockVectorStore::new());
    let ns = Namespace::from("test-scope");

    let index = IndexRun::new(
        Arc::new(TreeSitterChunker::new()),
        Arc::new(MockSummarizer::new()),
        Arc::new(MockEmbedder::new(8)),
        Arc::new(ArcStore(store.clone())),
        ns.clone(),
        1,
    );
    index.run(true, &dir).await.unwrap();

    let search = SearchRun::new(
        Box::new(MockEmbedder::new(8)),
        Box::new(ArcStore(store.clone())),
        ns.clone(),
    );

    // Scope to src/ — should exclude README.md
    let result = search
        .run(&SearchParams {
            query: "anything".into(),
            top_k: 20,
            symbols_per_file: 10,
            no_symbols: false,
            scope: vec!["src/".into()],
            filters: vec![],
        })
        .await
        .unwrap();

    for file_result in &result.results {
        assert!(
            file_result.path.starts_with("src/"),
            "scoped result has unexpected path: {}",
            file_result.path
        );
    }

    cleanup(&dir);
}

#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn incremental_index_only_processes_changed_files() {
    let dir = create_fixture_repo("incremental");

    let store = Arc::new(MockVectorStore::new());
    let ns = Namespace::from("test-incr");

    // Full index
    let index = IndexRun::new(
        Arc::new(TreeSitterChunker::new()),
        Arc::new(MockSummarizer::new()),
        Arc::new(MockEmbedder::new(8)),
        Arc::new(ArcStore(store.clone())),
        ns.clone(),
        1,
    );
    let report1 = index.run(true, &dir).await.unwrap();
    let initial_count = report1.files_processed;

    // Add a new file and commit
    std::fs::write(
        dir.join("src/new_feature.rs"),
        "pub fn new_thing() { println!(\"new\"); }\n",
    )
    .unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-m", "add new feature"]);

    // Incremental index (full=false)
    let index2 = IndexRun::new(
        Arc::new(TreeSitterChunker::new()),
        Arc::new(MockSummarizer::new()),
        Arc::new(MockEmbedder::new(8)),
        Arc::new(ArcStore(store.clone())),
        ns.clone(),
        1,
    );
    let report2 = index2.run(false, &dir).await.unwrap();

    assert!(
        report2.files_processed < initial_count,
        "incremental should process fewer files ({} vs {})",
        report2.files_processed,
        initial_count
    );
    assert!(report2.files_processed >= 1, "should process the new file");

    cleanup(&dir);
}

#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn incremental_index_removes_stale_symbols() {
    let dir = create_fixture_repo("stale-syms");

    let store = Arc::new(MockVectorStore::new());
    let ns = Namespace::from("test-stale");

    // Full index — payments.rs has release_payment + process_refund
    let index = IndexRun::new(
        Arc::new(TreeSitterChunker::new()),
        Arc::new(MockSummarizer::new()),
        Arc::new(MockEmbedder::new(8)),
        Arc::new(ArcStore(store.clone())),
        ns.clone(),
        1,
    );
    index.run(true, &dir).await.unwrap();

    // Count vectors for payments.rs before modification
    let before = store.document_count(&ns, "src/payments.rs");
    assert!(before >= 3, "expected file + 2 symbols, got {before}");

    // Remove process_refund, keep release_payment
    std::fs::write(
        dir.join("src/payments.rs"),
        r#"use std::io;

/// Releases a commission payment to a payee.
pub fn release_payment(payee_id: u64, amount: f64) -> Result<(), String> {
    if amount <= 0.0 {
        return Err("amount must be positive".into());
    }
    println!("Releasing {amount} to payee {payee_id}");
    Ok(())
}
"#,
    )
    .unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-m", "remove process_refund"]);

    // Incremental index
    let index2 = IndexRun::new(
        Arc::new(TreeSitterChunker::new()),
        Arc::new(MockSummarizer::new()),
        Arc::new(MockEmbedder::new(8)),
        Arc::new(ArcStore(store.clone())),
        ns.clone(),
        1,
    );
    index2.run(false, &dir).await.unwrap();

    // After: should have fewer vectors (stale process_refund symbol removed)
    let after = store.document_count(&ns, "src/payments.rs");
    assert!(
        after < before,
        "expected fewer vectors after symbol removal: before={before}, after={after}"
    );
    // Should have exactly file + 1 symbol now
    assert_eq!(after, 2, "expected file + 1 symbol, got {after}");

    cleanup(&dir);
}

#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn json_output_is_valid_after_full_pipeline() {
    let dir = create_fixture_repo("json-output");

    let store = Arc::new(MockVectorStore::new());
    let ns = Namespace::from("test-json");

    let index = IndexRun::new(
        Arc::new(TreeSitterChunker::new()),
        Arc::new(MockSummarizer::new()),
        Arc::new(MockEmbedder::new(8)),
        Arc::new(ArcStore(store.clone())),
        ns.clone(),
        1,
    );
    index.run(true, &dir).await.unwrap();

    let search = SearchRun::new(
        Box::new(MockEmbedder::new(8)),
        Box::new(ArcStore(store.clone())),
        ns.clone(),
    );
    let result = search
        .run(&SearchParams {
            query: "test".into(),
            top_k: 5,
            symbols_per_file: 3,
            no_symbols: false,
            scope: vec![],
            filters: vec![],
        })
        .await
        .unwrap();

    let json_str = wdpkr::search::output::render_json(&result, false).unwrap();
    let json: serde_json::Value = serde_json::from_str(&json_str).unwrap();

    assert!(json["query"].is_string());
    assert!(json["namespace"].is_string());
    assert!(json["results"].is_array());

    cleanup(&dir);
}

// ── Arc wrapper for shared mock store ─────────────────────────────────────
// IndexRun and SearchRun take `Box<dyn VectorStore>` (owned). To share
// a single MockVectorStore between index and search, we wrap it in Arc
// and implement VectorStore on the wrapper by delegating to the inner.

struct ArcStore(Arc<MockVectorStore>);

#[async_trait::async_trait]
impl VectorStore for ArcStore {
    async fn create_namespace(&self, ns: &Namespace, dimension: usize) -> anyhow::Result<()> {
        self.0.create_namespace(ns, dimension).await
    }
    async fn delete_namespace(&self, ns: &Namespace) -> anyhow::Result<()> {
        self.0.delete_namespace(ns).await
    }
    async fn namespace_exists(&self, ns: &Namespace) -> anyhow::Result<bool> {
        self.0.namespace_exists(ns).await
    }
    async fn get_metadata(
        &self,
        ns: &Namespace,
    ) -> anyhow::Result<wdpkr::store::NamespaceMetadata> {
        self.0.get_metadata(ns).await
    }
    async fn set_metadata(
        &self,
        ns: &Namespace,
        meta: &wdpkr::store::NamespaceMetadata,
    ) -> anyhow::Result<()> {
        self.0.set_metadata(ns, meta).await
    }
    async fn upsert(
        &self,
        ns: &Namespace,
        docs: &[wdpkr::store::VectorDocument],
    ) -> anyhow::Result<wdpkr::store::UpsertStats> {
        self.0.upsert(ns, docs).await
    }
    async fn delete_by_ids(&self, ns: &Namespace, ids: &[&str]) -> anyhow::Result<()> {
        self.0.delete_by_ids(ns, ids).await
    }
    async fn delete_by_file(&self, ns: &Namespace, file_path: &str) -> anyhow::Result<()> {
        self.0.delete_by_file(ns, file_path).await
    }
    async fn delete_by_glob(&self, ns: &Namespace, pattern: &str) -> anyhow::Result<usize> {
        self.0.delete_by_glob(ns, pattern).await
    }
    async fn get_content_hashes(
        &self,
        ns: &Namespace,
    ) -> anyhow::Result<std::collections::HashMap<String, String>> {
        self.0.get_content_hashes(ns).await
    }
    async fn list_documents(
        &self,
        ns: &Namespace,
    ) -> anyhow::Result<Vec<wdpkr::store::VectorDocument>> {
        self.0.list_documents(ns).await
    }
    async fn search(
        &self,
        ns: &Namespace,
        query_vector: &[f32],
        opts: &wdpkr::store::SearchOptions,
    ) -> anyhow::Result<Vec<wdpkr::store::SearchResult>> {
        self.0.search(ns, query_vector, opts).await
    }
}
