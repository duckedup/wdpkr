//! End-to-end decision recall through the public library API (offline, mocks).
//!
//! Mirrors what `wdpkr decision add` + `wdpkr search` do, without the CLI or any
//! network: author a decision (embed → upsert into the `--decision` namespace +
//! persist the registry in namespace metadata), seed a code file, then run a
//! multi-namespace search and assert L1 (decision hit) and L2 (`governed_by`
//! attach), plus superseded exclusion after a superseding decision is added.

use wdpkr::config::DecayConfig;
use wdpkr::decision::{DecisionEntry, DecisionRegistry, DecisionStatus, REGISTRY_META_KEY};
use wdpkr::indexer::pipeline::{EmbedMode, process_item};
use wdpkr::search::{SearchParams, SearchRun, compile_decisions};
use wdpkr::store::{ChunkKind, Namespace, VectorDocument, VectorStore};
use wdpkr::testing::mock_embed::MockEmbedder;
use wdpkr::testing::mock_store::MockVectorStore;

const DIM: usize = 3;
const NOW: i64 = 1_000_000;
const HIT: [f32; 3] = [1.0, 0.0, 0.0];

fn code_doc(path: &str) -> VectorDocument {
    VectorDocument {
        id: format!("code-{path}"),
        vector: HIT.to_vec(),
        summary: format!("summary of {path}"),
        file_path: path.into(),
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
    }
}

fn entry(id: u32, title: &str, areas: &[&str]) -> DecisionEntry {
    DecisionEntry {
        id,
        title: title.into(),
        status: DecisionStatus::Accepted,
        author: "Ada".into(),
        date: NOW,
        updated_at: None,
        context: Some("why it matters".into()),
        decision: Some("what we chose".into()),
        consequences: Some("the trade-offs".into()),
        sources: vec![],
        areas: areas.iter().map(|s| s.to_string()).collect(),
        supersedes: vec![],
        overrides: vec![],
        relates_to: vec![],
        superseded_by: None,
    }
}

fn embedder() -> MockEmbedder {
    let mut e = MockEmbedder::new(DIM);
    e.set_override("q", HIT.to_vec());
    e
}

/// Author a decision the way the CLI does: embed its content, force the known
/// query vector so it deterministically matches, upsert to the decision ns.
async fn author(store: &MockVectorStore, ns: &Namespace, e: &DecisionEntry) {
    let item = e.to_source_item();
    let result = process_item(&item, None, &embedder(), EmbedMode::Docstring)
        .await
        .unwrap();
    let mut docs = result.documents;
    for d in &mut docs {
        d.vector = HIT.to_vec();
        d.last_used_at = Some(NOW);
    }
    store.delete_by_file(ns, &e.uri()).await.unwrap();
    store.upsert(ns, &docs).await.unwrap();
}

/// Build a store containing a finance code file plus the given authored
/// decisions, with `reg` persisted into the decision namespace metadata.
async fn scenario(decisions: &[DecisionEntry], reg: &DecisionRegistry) -> MockVectorStore {
    let store = MockVectorStore::new();
    let code_ns = Namespace::from("test");
    let dec_ns = Namespace::from("test--decision");
    store.create_namespace(&code_ns, DIM).await.unwrap();
    store.create_namespace(&dec_ns, DIM).await.unwrap();
    store
        .upsert(&code_ns, &[code_doc("src/finance/commission.rs")])
        .await
        .unwrap();
    for e in decisions {
        author(&store, &dec_ns, e).await;
    }
    let mut meta = store.get_metadata(&dec_ns).await.unwrap();
    meta.extra
        .insert(REGISTRY_META_KEY.into(), reg.to_json().unwrap());
    store.set_metadata(&dec_ns, &meta).await.unwrap();
    store
}

fn namespaces() -> Vec<(Namespace, Option<String>, DecayConfig)> {
    vec![
        (Namespace::from("test"), None, DecayConfig::default()),
        (
            Namespace::from("test--decision"),
            Some("decision".into()),
            DecayConfig::default(),
        ),
    ]
}

fn params() -> SearchParams {
    SearchParams {
        query: "q".into(),
        top_k: 10,
        symbols_per_file: 0,
        no_symbols: true,
        scope: vec![],
        filters: vec![],
    }
}

#[tokio::test]
async fn decision_recall_end_to_end() {
    // ── Author decision 1 governing src/finance/** ──
    let d1 = entry(1, "Half-up rounding", &["src/finance/**"]);
    let mut reg = DecisionRegistry::default();
    reg.upsert(d1.clone());
    let store = scenario(std::slice::from_ref(&d1), &reg).await;

    // Registry round-trips through the store's metadata.
    let dec_ns = Namespace::from("test--decision");
    let reloaded = {
        let meta = store.get_metadata(&dec_ns).await.unwrap();
        DecisionRegistry::from_json(meta.extra.get(REGISTRY_META_KEY).unwrap()).unwrap()
    };
    assert_eq!(reloaded, reg, "registry must survive store round-trip");

    // ── Search: L1 (decision hit) + L2 (governed_by) ──
    let decisions = compile_decisions(&reloaded).unwrap();
    let search =
        SearchRun::new_multi_with_decay(Box::new(embedder()), Box::new(store), namespaces(), NOW)
            .with_decisions(decisions);
    let report = search.run(&params()).await.unwrap();

    let commission = report
        .results
        .iter()
        .find(|r| r.path == "src/finance/commission.rs")
        .expect("code file present");
    let gov = commission
        .governed_by
        .as_ref()
        .expect("L2: governed_by attached to in-area code");
    assert_eq!(gov[0].path, "decision://0001");

    let decision_hit = report
        .results
        .iter()
        .find(|r| r.path == "decision://0001")
        .expect("L1: decision surfaces as its own hit");
    assert_eq!(decision_hit.source.as_deref(), Some("decision"));

    // ── Supersede: decision 2 replaces 1; 1 drops from active recall ──
    let d2 = entry(2, "Banker's rounding", &["src/finance/**"]);
    let mut reg2 = DecisionRegistry::default();
    reg2.upsert(d1);
    reg2.upsert(d2.clone());
    reg2.mark_superseded(1, 2);
    // Both decision docs remain in the store (superseded stays, walkable).
    let store2 = scenario(
        &[entry(1, "Half-up rounding", &["src/finance/**"]), d2],
        &reg2,
    )
    .await;

    let decisions2 = compile_decisions(&reg2).unwrap();
    let search2 =
        SearchRun::new_multi_with_decay(Box::new(embedder()), Box::new(store2), namespaces(), NOW)
            .with_decisions(decisions2);
    let report2 = search2.run(&params()).await.unwrap();

    let paths: Vec<&str> = report2.results.iter().map(|r| r.path.as_str()).collect();
    assert!(
        !paths.contains(&"decision://0001"),
        "superseded decision excluded from active recall"
    );
    assert!(paths.contains(&"decision://0002"), "successor is active");

    // governed_by now points to the active successor only.
    let commission2 = report2
        .results
        .iter()
        .find(|r| r.path == "src/finance/commission.rs")
        .unwrap();
    let gov2 = commission2.governed_by.as_ref().unwrap();
    let gov_paths: Vec<&str> = gov2.iter().map(|g| g.path.as_str()).collect();
    assert_eq!(gov_paths, vec!["decision://0002"]);
}
