//! End-to-end search integration test.
//!
//! Exercises the full pipeline through the public library API:
//! mock setup → SearchRun → SearchReport → JSON/pretty rendering.
//! Validates the SPEC's JSON contract from an external consumer's perspective.

use wdpkr::search::output;
use wdpkr::search::{SearchParams, SearchRun};
use wdpkr::store::{ChunkKind, Namespace, NamespaceMetadata, VectorDocument, VectorStore};
use wdpkr::testing::mock_embed::MockEmbedder;
use wdpkr::testing::mock_store::MockVectorStore;

async fn seeded_env() -> (MockVectorStore, MockEmbedder) {
    let store = MockVectorStore::new();
    store
        .create_namespace(&Namespace::from("integration"), 3)
        .await
        .unwrap();

    wdpkr::store::VectorStore::set_metadata(
        &store,
        &Namespace::from("integration"),
        &NamespaceMetadata {
            hwm_sha: Some("e2e-test-sha".into()),
            embedder: Some("mock/mock-embed-v1".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let docs = vec![
        VectorDocument {
            id: "f-payments".into(),
            vector: vec![1.0, 0.0, 0.0],
            summary: "Payment processing and release logic".into(),
            file_path: "src/finance/payments.rs".into(),
            chunk_kind: ChunkKind::File,
            symbol_name: None,
            symbol_kind: None,
            start_line: None,
            end_line: None,
            language: Some("rust".into()),
        },
        VectorDocument {
            id: "s-release".into(),
            vector: vec![0.95, 0.05, 0.0],
            summary: "Releases payment to a payee".into(),
            file_path: "src/finance/payments.rs".into(),
            chunk_kind: ChunkKind::Symbol,
            symbol_name: Some("release_payment".into()),
            symbol_kind: Some("function".into()),
            start_line: Some(10),
            end_line: Some(45),
            language: Some("rust".into()),
        },
        VectorDocument {
            id: "s-refund".into(),
            vector: vec![0.85, 0.15, 0.0],
            summary: "Processes a refund".into(),
            file_path: "src/finance/payments.rs".into(),
            chunk_kind: ChunkKind::Symbol,
            symbol_name: Some("process_refund".into()),
            symbol_kind: Some("function".into()),
            start_line: Some(50),
            end_line: Some(80),
            language: Some("rust".into()),
        },
        VectorDocument {
            id: "f-users".into(),
            vector: vec![0.0, 1.0, 0.0],
            summary: "User account management".into(),
            file_path: "src/users/accounts.rs".into(),
            chunk_kind: ChunkKind::File,
            symbol_name: None,
            symbol_kind: None,
            start_line: None,
            end_line: None,
            language: Some("rust".into()),
        },
        VectorDocument {
            id: "s-create-user".into(),
            vector: vec![0.05, 0.95, 0.0],
            summary: "Creates a new user account".into(),
            file_path: "src/users/accounts.rs".into(),
            chunk_kind: ChunkKind::Symbol,
            symbol_name: Some("create_user".into()),
            symbol_kind: Some("function".into()),
            start_line: Some(5),
            end_line: Some(25),
            language: Some("rust".into()),
        },
    ];

    wdpkr::store::VectorStore::upsert(&store, &Namespace::from("integration"), &docs)
        .await
        .unwrap();

    let mut embedder = MockEmbedder::new(3);
    embedder.set_override("payment processing", vec![0.9, 0.1, 0.0]);
    embedder.set_override("user accounts", vec![0.1, 0.9, 0.0]);
    embedder.set_override("something unrelated", vec![0.0, 0.0, 1.0]);

    (store, embedder)
}

fn search_run(embedder: MockEmbedder, store: MockVectorStore) -> SearchRun {
    SearchRun::new(
        Box::new(embedder),
        Box::new(store),
        Namespace::from("integration"),
    )
}

#[tokio::test]
async fn full_pipeline_json_matches_spec_contract() {
    let (store, embedder) = seeded_env().await;
    let search = search_run(embedder, store);

    let report = search
        .run(&SearchParams {
            query: "payment processing".into(),
            top_k: 5,
            symbols_per_file: 3,
            no_symbols: false,
            scope: None,
        })
        .await
        .unwrap();

    // Render to JSON and parse back — this is what an agent does.
    let json_str = output::render_json(&report).unwrap();
    let json: serde_json::Value = serde_json::from_str(&json_str).unwrap();

    // ── SPEC contract: top-level fields ──
    assert_eq!(json["query"], "payment processing");
    assert_eq!(json["namespace"], "integration");
    assert_eq!(json["indexed_at"], "e2e-test-sha");
    assert!(json["results"].is_array());

    // ── First result should be the payments file (closest vector) ──
    let first = &json["results"][0];
    assert_eq!(first["path"], "src/finance/payments.rs");
    assert!(first["score"].as_f64().unwrap() > 0.5);
    assert!(!first["summary"].as_str().unwrap().is_empty());

    // ── Symbols nested under the file ──
    let symbols = first["symbols"].as_array().unwrap();
    assert!(!symbols.is_empty());
    // Each symbol has the SPEC-required fields
    let sym = &symbols[0];
    assert!(sym["name"].is_string());
    assert!(sym["kind"].is_string());
    assert!(sym["lines"].is_array());
    assert_eq!(sym["lines"].as_array().unwrap().len(), 2);
    assert!(sym["summary"].is_string());
    assert!(sym["score"].is_number());
}

#[tokio::test]
async fn json_is_parseable_without_wdpkr_types() {
    // Simulates an agent parsing the JSON with only serde_json — no
    // wdpkr types imported. This catches accidentally non-serializable
    // fields or struct changes that break the contract.
    let (store, embedder) = seeded_env().await;
    let search = search_run(embedder, store);

    let report = search
        .run(&SearchParams {
            query: "payment processing".into(),
            top_k: 2,
            symbols_per_file: 2,
            no_symbols: false,
            scope: None,
        })
        .await
        .unwrap();

    let json_str = output::render_json(&report).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();

    // Walk the entire structure — if any field is missing or wrong type,
    // this will panic with a clear path.
    for result in parsed["results"].as_array().unwrap() {
        let _path = result["path"].as_str().unwrap();
        let _score = result["score"].as_f64().unwrap();
        let _summary = result["summary"].as_str().unwrap();
        for sym in result["symbols"].as_array().unwrap() {
            let _name = sym["name"].as_str().unwrap();
            let _kind = sym["kind"].as_str().unwrap();
            let _lines = sym["lines"].as_array().unwrap();
            let _summary = sym["summary"].as_str().unwrap();
            let _score = sym["score"].as_f64().unwrap();
        }
    }
}

#[tokio::test]
async fn pretty_output_contains_all_key_info() {
    let (store, embedder) = seeded_env().await;
    let search = search_run(embedder, store);

    let report = search
        .run(&SearchParams {
            query: "payment processing".into(),
            top_k: 5,
            symbols_per_file: 3,
            no_symbols: false,
            scope: None,
        })
        .await
        .unwrap();

    let pretty = output::render_pretty(&report);

    assert!(pretty.contains("src/finance/payments.rs"));
    assert!(pretty.contains("release_payment"));
    assert!(pretty.contains("process_refund"));
    assert!(pretty.contains("e2e-test-sha"));
}

#[tokio::test]
async fn scope_filters_end_to_end() {
    let (store, embedder) = seeded_env().await;
    let search = search_run(embedder, store);

    let report = search
        .run(&SearchParams {
            query: "payment processing".into(),
            top_k: 5,
            symbols_per_file: 3,
            no_symbols: false,
            scope: Some("src/finance/".into()),
        })
        .await
        .unwrap();

    // Only finance files should appear
    for result in &report.results {
        assert!(
            result.path.starts_with("src/finance/"),
            "unexpected path outside scope: {}",
            result.path
        );
    }
    assert!(!report.results.is_empty());
}

#[tokio::test]
async fn unrelated_query_returns_low_relevance() {
    let (store, embedder) = seeded_env().await;
    let search = search_run(embedder, store);

    let report = search
        .run(&SearchParams {
            query: "something unrelated".into(),
            top_k: 5,
            symbols_per_file: 3,
            no_symbols: false,
            scope: None,
        })
        .await
        .unwrap();

    // Results exist (the store returns what it has) but scores should
    // be low since [0,0,1] is orthogonal to the document vectors.
    if !report.results.is_empty() {
        assert!(
            report.results[0].score < 0.5,
            "unrelated query should have low score, got {}",
            report.results[0].score
        );
    }
}

#[tokio::test]
async fn embedder_mismatch_is_caught() {
    let (store, embedder) = seeded_env().await;

    // Tamper with the stored embedder identity
    wdpkr::store::VectorStore::set_metadata(
        &store,
        &Namespace::from("integration"),
        &NamespaceMetadata {
            hwm_sha: Some("sha".into()),
            embedder: Some("voyage/voyage-code-3".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let search = search_run(embedder, store);
    let err = search
        .run(&SearchParams {
            query: "anything".into(),
            top_k: 5,
            symbols_per_file: 3,
            no_symbols: false,
            scope: None,
        })
        .await
        .unwrap_err();

    let msg = err.to_string();
    assert!(msg.contains("embedder mismatch"), "got: {msg}");
    assert!(msg.contains("voyage/voyage-code-3"), "got: {msg}");
    assert!(msg.contains("mock/mock-embed-v1"), "got: {msg}");
}
