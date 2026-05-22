//! Deterministic [`Plugin`] for tests and the eval harness.
//!
//! Returns configured items from `fetch()`, with optional content-hash
//! skip detection. Pure Rust — no FFI, no I/O, Miri-safe.

use anyhow::Result;
use async_trait::async_trait;

use crate::plugin::{FetchContext, FetchResult, Plugin, SourceItem};

pub struct MockPlugin {
    name: String,
    items: Vec<SourceItem>,
    deletions: Vec<String>,
    cursor: Option<String>,
}

impl MockPlugin {
    pub fn new(name: &str, items: Vec<SourceItem>) -> Self {
        Self {
            name: name.to_string(),
            items,
            deletions: vec![],
            cursor: None,
        }
    }

    pub fn with_deletions(name: &str, items: Vec<SourceItem>, deletions: Vec<String>) -> Self {
        Self {
            name: name.to_string(),
            items,
            deletions,
            cursor: None,
        }
    }

    pub fn with_cursor(name: &str, items: Vec<SourceItem>, cursor: String) -> Self {
        Self {
            name: name.to_string(),
            items,
            deletions: vec![],
            cursor: Some(cursor),
        }
    }
}

#[async_trait]
impl Plugin for MockPlugin {
    fn name(&self) -> &str {
        &self.name
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<FetchResult> {
        let items = if ctx.full {
            self.items.clone()
        } else {
            self.items
                .iter()
                .filter(|item| {
                    ctx.stored_hashes
                        .get(&item.source_path)
                        .is_none_or(|h| *h != item.content_hash)
                })
                .cloned()
                .collect()
        };

        Ok(FetchResult {
            items,
            deletions: self.deletions.clone(),
            cursor: self.cursor.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_matches_constructor() {
        let p = MockPlugin::new("linear", vec![]);
        assert_eq!(p.name(), "linear");
    }

    #[test]
    fn default_deletions_empty() {
        let p = MockPlugin::new("test", vec![]);
        assert!(p.deletions.is_empty());
    }
}
