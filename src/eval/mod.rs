pub mod metrics;
pub mod output;
pub mod runner;

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct EvalCase {
    pub query: String,
    #[serde(default)]
    pub expected_files: Vec<String>,
    #[serde(default = "default_top_k")]
    pub top_k: usize,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EvalSuite {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub cases: Vec<EvalCase>,
}

fn default_top_k() -> usize {
    5
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_case_with_defaults() {
        let json = r#"{"query": "test query"}"#;
        let case: EvalCase = serde_json::from_str(json).unwrap();
        assert_eq!(case.query, "test query");
        assert_eq!(case.top_k, 5);
        assert!(case.expected_files.is_empty());
        assert!(case.label.is_none());
        assert!(case.tags.is_empty());
    }

    #[test]
    fn deserialize_case_all_fields() {
        let json = r#"{
            "query": "search pipeline",
            "expected_files": ["src/search/mod.rs"],
            "top_k": 10,
            "label": "search-test",
            "tags": ["search"]
        }"#;
        let case: EvalCase = serde_json::from_str(json).unwrap();
        assert_eq!(case.top_k, 10);
        assert_eq!(case.expected_files, vec!["src/search/mod.rs"]);
        assert_eq!(case.label.as_deref(), Some("search-test"));
        assert_eq!(case.tags, vec!["search"]);
    }

    #[test]
    fn deserialize_suite() {
        let json = r#"{
            "name": "test-suite",
            "description": "A test suite",
            "cases": [
                {"query": "q1"},
                {"query": "q2", "expected_files": ["a.rs"]}
            ]
        }"#;
        let suite: EvalSuite = serde_json::from_str(json).unwrap();
        assert_eq!(suite.name, "test-suite");
        assert_eq!(suite.cases.len(), 2);
    }
}
