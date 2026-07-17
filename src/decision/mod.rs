//! Store-native architectural decisions ("decision recall").
//!
//! A **decision** is an authored ADR-style memory object: it captures *why* the
//! code is shaped the way it is, optionally pulling provenance snapshots from
//! configured taps (notion, …). Decisions are not files on disk — they live in
//! the same vector store as the code index, in a dedicated `<base>--decision`
//! namespace, and are referenced back into search results as `governed_by` on
//! the code they govern.
//!
//! This module owns the pure, side-effect-free pieces: the decision **registry**
//! (metadata for every decision, persisted as JSON inside the decision
//! namespace's [`NamespaceMetadata::extra`](crate::store::NamespaceMetadata)) and
//! the assembly of a decision into an embeddable [`SourceItem`]. The CLI
//! (`src/cli/decision.rs`) drives the store I/O; the search layer reads the
//! registry to attach governing decisions.

use std::fmt;
use std::str::FromStr;

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

use crate::tap::{SourceChunk, SourceItem};

/// URI scheme for a decision's `source_path`. Must equal [`TAP_NAME`] so
/// `reinforce`/`delete`/`search --tap` route to the decision namespace via
/// `namespace_suffix` (see `src/cli/reinforce.rs`).
pub const SOURCE_SCHEME: &str = "decision://";

/// Pseudo-tap name for the decision namespace (`<base>--decision`).
pub const TAP_NAME: &str = "decision";

/// Key under which the JSON registry is stored in the decision namespace's
/// `NamespaceMetadata.extra`.
pub const REGISTRY_META_KEY: &str = "decisions";

/// The stable URI for a decision id, e.g. `decision://0007`.
pub fn decision_uri(id: u32) -> String {
    format!("{SOURCE_SCHEME}{id:04}")
}

// ── Status ──────────────────────────────────────────────────────────────────

/// Lifecycle status of a decision. Active decisions (`Proposed`/`Accepted`)
/// participate in recall; `Superseded`/`Deprecated` ones are dropped from active
/// results but remain in the store and walkable via relationship links.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionStatus {
    Proposed,
    #[default]
    Accepted,
    Superseded,
    Deprecated,
}

impl DecisionStatus {
    /// Whether this status participates in active recall.
    pub fn is_active(self) -> bool {
        matches!(self, Self::Proposed | Self::Accepted)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Proposed => "proposed",
            Self::Accepted => "accepted",
            Self::Superseded => "superseded",
            Self::Deprecated => "deprecated",
        }
    }
}

impl fmt::Display for DecisionStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for DecisionStatus {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "proposed" => Ok(Self::Proposed),
            "accepted" => Ok(Self::Accepted),
            "superseded" => Ok(Self::Superseded),
            "deprecated" => Ok(Self::Deprecated),
            other => Err(anyhow!(
                "unknown decision status '{other}' (expected proposed|accepted|superseded|deprecated)"
            )),
        }
    }
}

// ── Source reference ─────────────────────────────────────────────────────────

/// A pulled provenance source: the tap URI it came from plus a snapshot of its
/// content at pull time. The snapshot keeps the decision self-contained and is
/// embedded for recall; the URI lets an agent re-fetch the live source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRef {
    pub uri: String,
    pub snapshot: String,
}

// ── Decision entry ───────────────────────────────────────────────────────────

/// One decision. This is the source of truth for a decision's content and
/// metadata; the embedded [`crate::store::VectorDocument`]s are derived from it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionEntry {
    pub id: u32,
    pub title: String,
    #[serde(default)]
    pub status: DecisionStatus,
    /// Who made the decision (traceable attribution).
    pub author: String,
    /// Unix seconds when created.
    pub date: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<i64>,

    // ── authored body ──
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consequences: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<SourceRef>,

    // ── recall + relationships ──
    /// Code path globs this decision governs (drives L2 `governed_by` attach).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub areas: Vec<String>,
    /// Decisions this one replaces; each is marked `Superseded` on write.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supersedes: Vec<u32>,
    /// Decisions this one wins over in overlapping areas (scope-override).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overrides: Vec<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relates_to: Vec<u32>,
    /// Backlink: the decision that superseded this one, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<u32>,
}

impl DecisionEntry {
    pub fn uri(&self) -> String {
        decision_uri(self.id)
    }

    /// The provenance source URIs (without snapshots).
    pub fn source_uris(&self) -> Vec<String> {
        self.sources.iter().map(|s| s.uri.clone()).collect()
    }

    /// Assemble this decision into an embeddable [`SourceItem`]: a doc-level
    /// markdown body keyed `decision://{id}`, with one section child per
    /// non-empty part (Context / Decision / Consequences / each Source).
    pub fn to_source_item(&self) -> SourceItem {
        let mut children: Vec<SourceChunk> = Vec::new();
        let mut body = String::new();
        body.push_str(&format!("# {}\n\n", self.title));

        let mut add_section = |name: &str, content: &str, body: &mut String| {
            let trimmed = content.trim();
            if trimmed.is_empty() {
                return;
            }
            body.push_str(&format!("## {name}\n{trimmed}\n\n"));
            children.push(SourceChunk {
                name: name.to_string(),
                kind: "section".to_string(),
                content: trimmed.to_string(),
                signature: None,
                doc_comment: None,
                start_line: None,
                end_line: None,
                references: Vec::new(),
            });
        };

        add_section("Context", self.context.as_deref().unwrap_or(""), &mut body);
        add_section(
            "Decision",
            self.decision.as_deref().unwrap_or(""),
            &mut body,
        );
        add_section(
            "Consequences",
            self.consequences.as_deref().unwrap_or(""),
            &mut body,
        );
        for src in &self.sources {
            add_section(&format!("Source: {}", src.uri), &src.snapshot, &mut body);
        }

        let content = body.trim_end().to_string();
        let content_hash = blake3::hash(content.as_bytes()).to_hex()[..16].to_string();

        SourceItem {
            source_path: self.uri(),
            content,
            content_hash,
            language: None,
            module_doc: None,
            children,
        }
    }
}

// ── Registry ─────────────────────────────────────────────────────────────────

/// The set of all decisions in a namespace. Serialized to JSON and stored in
/// `NamespaceMetadata.extra[REGISTRY_META_KEY]`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DecisionRegistry {
    #[serde(default)]
    pub decisions: Vec<DecisionEntry>,
}

impl DecisionRegistry {
    pub fn from_json(s: &str) -> Result<Self> {
        Ok(serde_json::from_str(s)?)
    }

    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string(self)?)
    }

    /// Next free id: max existing +1 (ids start at 1).
    pub fn next_id(&self) -> u32 {
        self.decisions.iter().map(|d| d.id).max().unwrap_or(0) + 1
    }

    pub fn get(&self, id: u32) -> Option<&DecisionEntry> {
        self.decisions.iter().find(|d| d.id == id)
    }

    pub fn get_mut(&mut self, id: u32) -> Option<&mut DecisionEntry> {
        self.decisions.iter_mut().find(|d| d.id == id)
    }

    /// Insert or replace an entry, keeping the list sorted by id.
    pub fn upsert(&mut self, entry: DecisionEntry) {
        match self.decisions.iter().position(|d| d.id == entry.id) {
            Some(pos) => self.decisions[pos] = entry,
            None => self.decisions.push(entry),
        }
        self.decisions.sort_by_key(|d| d.id);
    }

    /// Remove a decision and scrub every dangling link to it in other entries.
    /// Returns the removed entry if present.
    pub fn remove(&mut self, id: u32) -> Option<DecisionEntry> {
        let pos = self.decisions.iter().position(|d| d.id == id)?;
        let removed = self.decisions.remove(pos);
        self.scrub_links(id);
        Some(removed)
    }

    /// Drop all references to `id` from other entries' link fields.
    pub fn scrub_links(&mut self, id: u32) {
        for d in &mut self.decisions {
            d.supersedes.retain(|x| *x != id);
            d.overrides.retain(|x| *x != id);
            d.relates_to.retain(|x| *x != id);
            if d.superseded_by == Some(id) {
                d.superseded_by = None;
            }
        }
    }

    /// Mark `old` as superseded by `by`.
    pub fn mark_superseded(&mut self, old: u32, by: u32) {
        if let Some(e) = self.get_mut(old) {
            e.status = DecisionStatus::Superseded;
            e.superseded_by = Some(by);
        }
    }

    /// Active (recall-participating) decisions.
    pub fn active(&self) -> impl Iterator<Item = &DecisionEntry> {
        self.decisions.iter().filter(|d| d.status.is_active())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: u32) -> DecisionEntry {
        DecisionEntry {
            id,
            title: format!("Decision {id}"),
            status: DecisionStatus::Accepted,
            author: "Ada".into(),
            date: 1000,
            updated_at: None,
            context: Some("why".into()),
            decision: Some("what".into()),
            consequences: Some("so".into()),
            sources: vec![],
            areas: vec!["src/**".into()],
            supersedes: vec![],
            overrides: vec![],
            relates_to: vec![],
            superseded_by: None,
        }
    }

    #[test]
    fn uri_is_zero_padded() {
        assert_eq!(decision_uri(7), "decision://0007");
        assert_eq!(entry(42).uri(), "decision://0042");
    }

    #[test]
    fn scheme_equals_tap_name() {
        assert_eq!(SOURCE_SCHEME, format!("{TAP_NAME}://"));
    }

    #[test]
    fn status_round_trips_and_parses() {
        for s in [
            DecisionStatus::Proposed,
            DecisionStatus::Accepted,
            DecisionStatus::Superseded,
            DecisionStatus::Deprecated,
        ] {
            assert_eq!(s.as_str().parse::<DecisionStatus>().unwrap(), s);
        }
        assert!("Accepted".parse::<DecisionStatus>().is_ok());
        assert!("bogus".parse::<DecisionStatus>().is_err());
    }

    #[test]
    fn status_active_flags() {
        assert!(DecisionStatus::Proposed.is_active());
        assert!(DecisionStatus::Accepted.is_active());
        assert!(!DecisionStatus::Superseded.is_active());
        assert!(!DecisionStatus::Deprecated.is_active());
    }

    #[test]
    fn registry_json_round_trip() {
        let mut reg = DecisionRegistry::default();
        reg.upsert(entry(1));
        reg.upsert(entry(2));
        let json = reg.to_json().unwrap();
        let back = DecisionRegistry::from_json(&json).unwrap();
        assert_eq!(reg, back);
    }

    #[test]
    fn next_id_increments_from_max() {
        let mut reg = DecisionRegistry::default();
        assert_eq!(reg.next_id(), 1);
        reg.upsert(entry(1));
        reg.upsert(entry(5));
        assert_eq!(reg.next_id(), 6);
    }

    #[test]
    fn upsert_replaces_and_sorts() {
        let mut reg = DecisionRegistry::default();
        reg.upsert(entry(3));
        reg.upsert(entry(1));
        let mut two = entry(1);
        two.title = "updated".into();
        reg.upsert(two);
        assert_eq!(reg.decisions.len(), 2);
        assert_eq!(reg.decisions[0].id, 1);
        assert_eq!(reg.decisions[0].title, "updated");
        assert_eq!(reg.decisions[1].id, 3);
    }

    #[test]
    fn remove_scrubs_links() {
        let mut reg = DecisionRegistry::default();
        let mut a = entry(1);
        a.supersedes = vec![2];
        a.overrides = vec![2, 3];
        a.relates_to = vec![2];
        let mut b = entry(2);
        b.superseded_by = Some(1);
        reg.upsert(a);
        reg.upsert(b);
        reg.upsert(entry(3));

        let removed = reg.remove(2).unwrap();
        assert_eq!(removed.id, 2);
        let one = reg.get(1).unwrap();
        assert!(one.supersedes.is_empty());
        assert_eq!(one.overrides, vec![3]);
        assert!(one.relates_to.is_empty());
    }

    #[test]
    fn mark_superseded_sets_status_and_backlink() {
        let mut reg = DecisionRegistry::default();
        reg.upsert(entry(1));
        reg.mark_superseded(1, 2);
        let e = reg.get(1).unwrap();
        assert_eq!(e.status, DecisionStatus::Superseded);
        assert_eq!(e.superseded_by, Some(2));
        assert_eq!(reg.active().count(), 0);
    }

    #[test]
    fn to_source_item_builds_sections() {
        let mut e = entry(7);
        e.sources = vec![SourceRef {
            uri: "notion://abc".into(),
            snapshot: "Half-up at 2 decimals".into(),
        }];
        let item = e.to_source_item();
        assert_eq!(item.source_path, "decision://0007");
        assert!(item.language.is_none());
        assert!(!item.content_hash.is_empty());
        // title + Context + Decision + Consequences + one Source section
        assert_eq!(item.children.len(), 4);
        assert!(item.content.contains("# Decision 7"));
        assert!(item.content.contains("## Context"));
        assert!(item.content.contains("## Source: notion://abc"));
        assert!(item.content.contains("Half-up at 2 decimals"));
    }

    #[test]
    fn to_source_item_omits_empty_sections() {
        let mut e = entry(1);
        e.context = None;
        e.consequences = Some("   ".into());
        let item = e.to_source_item();
        // only Decision remains a section
        assert_eq!(item.children.len(), 1);
        assert_eq!(item.children[0].name, "Decision");
    }

    #[test]
    fn content_hash_is_content_sensitive() {
        let mut a = entry(1);
        let mut b = entry(1);
        a.decision = Some("x".into());
        b.decision = Some("y".into());
        assert_ne!(
            a.to_source_item().content_hash,
            b.to_source_item().content_hash
        );
    }
}
