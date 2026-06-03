//! Built-in Linear tap: fetches the newest Linear issues (with their comment
//! threads) from the Linear GraphQL API and produces one [`SourceItem`] per
//! issue for the shared summarize → embed → upsert pipeline.
//!
//! The goal is *decision context* — descriptions and discussion are where the
//! "why" of past work lives, so the whole comment thread is folded into the
//! issue's document content and captured by a single file-level summary (one
//! summarizer call per issue, never one-per-comment).
//!
//! ## Index-state model
//!
//! Each run fetches the newest `amount` issues (the *complete desired set*),
//! so the index is reconciled to mirror exactly that set: any previously
//! indexed `linear://` document not in the freshly-fetched active set is
//! deleted. That uniformly prunes **archived**, **trashed/deleted**,
//! **permanently-purged**, and **aged-out** issues. Deletions are only
//! computed when the fetch fully succeeds, so a transient API failure can
//! never wipe the index.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use super::{FetchContext, FetchResult, SourceItem, Tap};

const DEFAULT_AMOUNT: usize = 50;
const DEFAULT_ORDER_BY: &str = "updatedAt";
const DEFAULT_API_KEY_ENV: &str = "LINEAR_API_KEY";
/// Per-request page size. Linear caps `first` at 250; 50 keeps payloads modest
/// while paginating up to `amount`.
const PAGE_SIZE: u32 = 50;
/// URI scheme used for a Linear issue's `source_path`.
pub const SOURCE_SCHEME: &str = "linear://";

// ── Settings ───────────────────────────────────────────────────────────────

/// Typed view of the `linear` tap's `settings` map.
#[derive(Debug, Clone)]
pub struct LinearTapSettings {
    /// How many of the newest issues to ingest.
    pub amount: usize,
    /// `"updatedAt"` (default) or `"createdAt"` — Linear `PaginationOrderBy`,
    /// which returns most-recent-first.
    pub order_by: String,
    /// Optional team-key filter (e.g. `"ENG"`).
    pub team: Option<String>,
    /// Fold each issue's comment thread into its document content.
    pub include_comments: bool,
    /// Name of the env var holding the Linear API key.
    pub api_key_env: String,
}

impl Default for LinearTapSettings {
    fn default() -> Self {
        Self {
            amount: DEFAULT_AMOUNT,
            order_by: DEFAULT_ORDER_BY.to_string(),
            team: None,
            include_comments: true,
            api_key_env: DEFAULT_API_KEY_ENV.to_string(),
        }
    }
}

impl LinearTapSettings {
    /// Parse from the tap's untyped `settings` map, validating types and the
    /// `order_by` enum. Unknown keys are ignored.
    pub fn from_settings(settings: &HashMap<String, serde_yaml::Value>) -> Result<Self> {
        let mut out = Self::default();

        if let Some(v) = settings.get("amount") {
            out.amount = v
                .as_u64()
                .ok_or_else(|| anyhow!("linear tap: 'amount' must be a positive integer"))?
                as usize;
        }
        if let Some(v) = settings.get("order_by") {
            let s = v
                .as_str()
                .ok_or_else(|| anyhow!("linear tap: 'order_by' must be a string"))?;
            if s != "updatedAt" && s != "createdAt" {
                return Err(anyhow!(
                    "linear tap: 'order_by' must be 'updatedAt' or 'createdAt', got '{s}'"
                ));
            }
            out.order_by = s.to_string();
        }
        if let Some(v) = settings.get("team") {
            out.team = Some(
                v.as_str()
                    .ok_or_else(|| anyhow!("linear tap: 'team' must be a string"))?
                    .to_string(),
            );
        }
        if let Some(v) = settings.get("include_comments") {
            out.include_comments = v
                .as_bool()
                .ok_or_else(|| anyhow!("linear tap: 'include_comments' must be a boolean"))?;
        }
        if let Some(v) = settings.get("api_key_env") {
            out.api_key_env = v
                .as_str()
                .ok_or_else(|| anyhow!("linear tap: 'api_key_env' must be a string"))?
                .to_string();
        }
        Ok(out)
    }
}

// ── GraphQL response types (wdpkr-owned) ─────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
struct Named {
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct Person {
    #[serde(rename = "displayName", default)]
    display_name: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

impl Person {
    fn label(&self) -> Option<&str> {
        self.display_name.as_deref().or(self.name.as_deref())
    }
}

#[derive(Debug, Clone, Deserialize)]
struct LabelConnection {
    #[serde(default)]
    nodes: Vec<Named>,
}

#[derive(Debug, Clone, Deserialize)]
struct Comment {
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    user: Option<Person>,
}

#[derive(Debug, Clone, Deserialize)]
struct CommentConnection {
    #[serde(default)]
    nodes: Vec<Comment>,
}

#[derive(Debug, Clone, Deserialize)]
struct Issue {
    identifier: String,
    title: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(rename = "priorityLabel", default)]
    priority_label: Option<String>,
    #[serde(rename = "updatedAt", default)]
    updated_at: Option<String>,
    #[serde(rename = "archivedAt", default)]
    archived_at: Option<String>,
    #[serde(default)]
    trashed: Option<bool>,
    #[serde(default)]
    state: Option<Named>,
    #[serde(default)]
    assignee: Option<Person>,
    #[serde(default)]
    team: Option<Named>,
    #[serde(default)]
    project: Option<Named>,
    #[serde(default)]
    labels: Option<LabelConnection>,
    #[serde(default)]
    comments: Option<CommentConnection>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct PageInfo {
    #[serde(rename = "hasNextPage", default)]
    has_next_page: bool,
    #[serde(rename = "endCursor", default)]
    end_cursor: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct IssueConnection {
    #[serde(default)]
    nodes: Vec<Issue>,
    #[serde(rename = "pageInfo", default)]
    page_info: PageInfo,
}

#[derive(Debug, Clone, Deserialize)]
struct IssuesResponse {
    issues: IssueConnection,
}

const ISSUES_QUERY: &str = r#"
query($first: Int!, $after: String, $orderBy: PaginationOrderBy, $filter: IssueFilter, $includeArchived: Boolean) {
  issues(first: $first, after: $after, orderBy: $orderBy, filter: $filter, includeArchived: $includeArchived) {
    nodes {
      identifier
      title
      description
      url
      priorityLabel
      updatedAt
      archivedAt
      trashed
      state { name }
      assignee { name displayName }
      team { name }
      project { name }
      labels { nodes { name } }
      comments(first: 100) { nodes { body user { name displayName } } }
    }
    pageInfo { hasNextPage endCursor }
  }
}
"#;

// ── Issue fetching (mockable) ────────────────────────────────────────────────

/// One page of issues from the Linear API. Abstracted so the tap's
/// orchestration (pagination, hash-skip, reconcile) is unit-testable with no
/// HTTP.
#[async_trait]
trait IssueFetcher: Send + Sync {
    async fn fetch_page(
        &self,
        first: u32,
        after: Option<String>,
        order_by: &str,
        team: Option<&str>,
    ) -> Result<IssueConnection>;
}

/// Real fetcher backed by linear-mg's GraphQL client.
struct LinearApiFetcher {
    client: linear_mg::client::LinearClient,
}

#[async_trait]
impl IssueFetcher for LinearApiFetcher {
    async fn fetch_page(
        &self,
        first: u32,
        after: Option<String>,
        order_by: &str,
        team: Option<&str>,
    ) -> Result<IssueConnection> {
        let filter = match team {
            Some(key) => json!({ "team": { "key": { "eq": key } } }),
            None => serde_json::Value::Null,
        };
        let variables = json!({
            "first": first,
            "after": after,
            "orderBy": order_by,
            "filter": filter,
            // Always include archived/trashed so they surface for pruning.
            "includeArchived": true,
        });
        let resp: IssuesResponse = self.client.query(ISSUES_QUERY, Some(variables)).await?;
        Ok(resp.issues)
    }
}

// ── Tap ──────────────────────────────────────────────────────────────────────

pub struct LinearTap {
    settings: LinearTapSettings,
    fetcher: Box<dyn IssueFetcher>,
}

impl LinearTap {
    /// Build from the tap's `settings` map, resolving the API key from the
    /// configured env var (default `LINEAR_API_KEY`).
    pub fn from_settings(settings: &HashMap<String, serde_yaml::Value>) -> Result<Self> {
        let settings = LinearTapSettings::from_settings(settings)?;
        let api_key = std::env::var(&settings.api_key_env).map_err(|_| {
            anyhow!(
                "Linear API key not set: export {} (or set the linear tap's settings.api_key_env)",
                settings.api_key_env
            )
        })?;
        let client = linear_mg::client::LinearClient::new(api_key);
        Ok(Self {
            settings,
            fetcher: Box::new(LinearApiFetcher { client }),
        })
    }

    #[cfg(test)]
    fn with_fetcher(settings: LinearTapSettings, fetcher: Box<dyn IssueFetcher>) -> Self {
        Self { settings, fetcher }
    }

    /// Paginate the newest `amount` issues (ordered by `order_by`, optional
    /// team filter, archived included for pruning).
    async fn fetch_issues(&self) -> Result<Vec<Issue>> {
        let mut collected: Vec<Issue> = Vec::new();
        let mut after: Option<String> = None;

        while collected.len() < self.settings.amount {
            let remaining = self.settings.amount - collected.len();
            let first = (remaining.min(PAGE_SIZE as usize)) as u32;
            let conn = self
                .fetcher
                .fetch_page(
                    first,
                    after.clone(),
                    &self.settings.order_by,
                    self.settings.team.as_deref(),
                )
                .await?;
            let got = conn.nodes.len();
            collected.extend(conn.nodes);
            if got == 0 || !conn.page_info.has_next_page || conn.page_info.end_cursor.is_none() {
                break;
            }
            after = conn.page_info.end_cursor;
        }

        collected.truncate(self.settings.amount);
        Ok(collected)
    }
}

#[async_trait]
impl Tap for LinearTap {
    fn name(&self) -> &str {
        "linear"
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<FetchResult> {
        let issues = self.fetch_issues().await?;

        let mut active_ids: HashSet<String> = HashSet::new();
        let mut items: Vec<SourceItem> = Vec::new();

        for issue in &issues {
            if is_gone(issue) {
                continue;
            }
            let item = issue_to_source_item(issue, &self.settings);
            active_ids.insert(item.source_path.clone());

            // Skip unchanged issues on incremental runs — but they stay in
            // `active_ids` so reconcile won't delete them.
            if !ctx.full
                && ctx
                    .stored_hashes
                    .get(&item.source_path)
                    .is_some_and(|h| *h == item.content_hash)
            {
                continue;
            }
            items.push(item);
        }

        // Full reconcile: anything indexed but no longer in the active set is
        // pruned (archived, trashed, purged, or aged out of the newest-N window).
        let deletions = reconcile_deletions(&ctx.stored_hashes, &active_ids);

        let cursor = issues.iter().filter_map(|i| i.updated_at.clone()).max();

        Ok(FetchResult {
            items,
            deletions,
            cursor,
        })
    }
}

// ── Pure helpers (no client, Miri-friendly) ──────────────────────────────────

/// Whether an issue should be removed from / kept out of the index.
fn is_gone(issue: &Issue) -> bool {
    issue.archived_at.is_some() || issue.trashed == Some(true)
}

/// Render an issue into the text that gets summarized + embedded: a metadata
/// digest, the description, and (optionally) the full comment thread.
fn render_issue_content(issue: &Issue, settings: &LinearTapSettings) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "{} {}", issue.identifier, issue.title);

    if let Some(state) = issue.state.as_ref().and_then(|n| n.name.as_deref()) {
        let _ = writeln!(s, "State: {state}");
    }
    if let Some(p) = issue.priority_label.as_deref().filter(|p| !p.is_empty()) {
        let _ = writeln!(s, "Priority: {p}");
    }
    if let Some(team) = issue.team.as_ref().and_then(|n| n.name.as_deref()) {
        let _ = writeln!(s, "Team: {team}");
    }
    if let Some(project) = issue.project.as_ref().and_then(|n| n.name.as_deref()) {
        let _ = writeln!(s, "Project: {project}");
    }
    if let Some(assignee) = issue.assignee.as_ref().and_then(Person::label) {
        let _ = writeln!(s, "Assignee: {assignee}");
    }
    let labels: Vec<&str> = issue
        .labels
        .as_ref()
        .map(|l| l.nodes.iter().filter_map(|n| n.name.as_deref()).collect())
        .unwrap_or_default();
    if !labels.is_empty() {
        let _ = writeln!(s, "Labels: {}", labels.join(", "));
    }
    if let Some(url) = issue.url.as_deref().filter(|u| !u.is_empty()) {
        let _ = writeln!(s, "URL: {url}");
    }
    if let Some(desc) = issue.description.as_deref().filter(|d| !d.is_empty()) {
        let _ = write!(s, "\n{desc}\n");
    }

    if settings.include_comments
        && let Some(comments) = issue.comments.as_ref()
    {
        let rendered: Vec<(&str, &str)> = comments
            .nodes
            .iter()
            .filter_map(|c| {
                let body = c.body.as_deref().filter(|b| !b.is_empty())?;
                let who = c.user.as_ref().and_then(Person::label).unwrap_or("unknown");
                Some((who, body))
            })
            .collect();
        if !rendered.is_empty() {
            let _ = writeln!(s, "\n## Comments");
            for (who, body) in rendered {
                let _ = writeln!(s, "\n— {who}:\n{body}");
            }
        }
    }

    s
}

/// Build a [`SourceItem`] from an issue: one childless document keyed by
/// `linear://{identifier}`.
fn issue_to_source_item(issue: &Issue, settings: &LinearTapSettings) -> SourceItem {
    let content = render_issue_content(issue, settings);
    let content_hash = blake3::hash(content.as_bytes()).to_hex()[..16].to_string();
    SourceItem {
        source_path: format!("{SOURCE_SCHEME}{}", issue.identifier),
        content,
        content_hash,
        language: None,
        module_doc: None,
        children: vec![],
    }
}

/// Stored `linear://` paths that are no longer in the active set — i.e. issues
/// that were archived, trashed, permanently purged, or aged out of the
/// newest-`amount` window. These get deleted from the namespace.
fn reconcile_deletions(stored: &HashMap<String, String>, active: &HashSet<String>) -> Vec<String> {
    stored
        .keys()
        .filter(|k| k.starts_with(SOURCE_SCHEME) && !active.contains(*k))
        .cloned()
        .collect()
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn issue(identifier: &str, title: &str) -> Issue {
        Issue {
            identifier: identifier.into(),
            title: title.into(),
            description: None,
            url: None,
            priority_label: None,
            updated_at: None,
            archived_at: None,
            trashed: None,
            state: None,
            assignee: None,
            team: None,
            project: None,
            labels: None,
            comments: None,
        }
    }

    // ── settings parsing ──────────────────────────────────────────────

    #[test]
    fn settings_defaults() {
        let s = LinearTapSettings::from_settings(&HashMap::new()).unwrap();
        assert_eq!(s.amount, DEFAULT_AMOUNT);
        assert_eq!(s.order_by, "updatedAt");
        assert!(s.team.is_none());
        assert!(s.include_comments);
        assert_eq!(s.api_key_env, "LINEAR_API_KEY");
    }

    #[test]
    fn settings_overrides() {
        let mut m = HashMap::new();
        m.insert("amount".into(), serde_yaml::Value::Number(100.into()));
        m.insert(
            "order_by".into(),
            serde_yaml::Value::String("createdAt".into()),
        );
        m.insert("team".into(), serde_yaml::Value::String("ENG".into()));
        m.insert("include_comments".into(), serde_yaml::Value::Bool(false));
        m.insert(
            "api_key_env".into(),
            serde_yaml::Value::String("MY_KEY".into()),
        );
        let s = LinearTapSettings::from_settings(&m).unwrap();
        assert_eq!(s.amount, 100);
        assert_eq!(s.order_by, "createdAt");
        assert_eq!(s.team.as_deref(), Some("ENG"));
        assert!(!s.include_comments);
        assert_eq!(s.api_key_env, "MY_KEY");
    }

    #[test]
    fn settings_rejects_bad_order_by() {
        let mut m = HashMap::new();
        m.insert(
            "order_by".into(),
            serde_yaml::Value::String("priority".into()),
        );
        let err = LinearTapSettings::from_settings(&m).unwrap_err();
        assert!(err.to_string().contains("order_by"), "got: {err}");
    }

    #[test]
    fn settings_rejects_wrong_type() {
        let mut m = HashMap::new();
        m.insert("amount".into(), serde_yaml::Value::String("lots".into()));
        let err = LinearTapSettings::from_settings(&m).unwrap_err();
        assert!(err.to_string().contains("amount"), "got: {err}");
    }

    // ── render / convert ──────────────────────────────────────────────

    #[test]
    fn render_includes_metadata_and_description() {
        let mut i = issue("ENG-1", "Fix login");
        i.description = Some("Users cannot log in after token refresh.".into());
        i.state = Some(Named {
            name: Some("In Progress".into()),
        });
        i.priority_label = Some("Urgent".into());
        i.labels = Some(LabelConnection {
            nodes: vec![Named {
                name: Some("bug".into()),
            }],
        });
        let s = render_issue_content(&i, &LinearTapSettings::default());
        assert!(s.contains("ENG-1 Fix login"));
        assert!(s.contains("State: In Progress"));
        assert!(s.contains("Priority: Urgent"));
        assert!(s.contains("Labels: bug"));
        assert!(s.contains("Users cannot log in"));
    }

    #[test]
    fn render_includes_comments_when_enabled() {
        let mut i = issue("ENG-2", "Rate table change");
        i.comments = Some(CommentConnection {
            nodes: vec![Comment {
                body: Some("We chose monthly buckets for cost reasons.".into()),
                user: Some(Person {
                    display_name: Some("Ada".into()),
                    name: None,
                }),
            }],
        });
        let s = render_issue_content(&i, &LinearTapSettings::default());
        assert!(s.contains("## Comments"));
        assert!(s.contains("Ada"));
        assert!(s.contains("monthly buckets"));
    }

    #[test]
    fn render_omits_comments_when_disabled() {
        let mut i = issue("ENG-3", "No comments wanted");
        i.comments = Some(CommentConnection {
            nodes: vec![Comment {
                body: Some("secret discussion".into()),
                user: None,
            }],
        });
        let settings = LinearTapSettings {
            include_comments: false,
            ..LinearTapSettings::default()
        };
        let s = render_issue_content(&i, &settings);
        assert!(!s.contains("secret discussion"));
        assert!(!s.contains("## Comments"));
    }

    #[test]
    fn source_item_keys_on_identifier_and_is_childless() {
        let item = issue_to_source_item(&issue("ENG-9", "X"), &LinearTapSettings::default());
        assert_eq!(item.source_path, "linear://ENG-9");
        assert!(item.children.is_empty());
        assert!(item.language.is_none());
        assert!(!item.content_hash.is_empty());
    }

    #[test]
    fn content_hash_is_deterministic_and_content_sensitive() {
        let a = issue_to_source_item(&issue("ENG-1", "Same"), &LinearTapSettings::default());
        let b = issue_to_source_item(&issue("ENG-1", "Same"), &LinearTapSettings::default());
        let c = issue_to_source_item(&issue("ENG-1", "Different"), &LinearTapSettings::default());
        assert_eq!(a.content_hash, b.content_hash);
        assert_ne!(a.content_hash, c.content_hash);
    }

    // ── is_gone ────────────────────────────────────────────────────────

    #[test]
    fn is_gone_detects_archived_and_trashed() {
        let mut archived = issue("ENG-1", "x");
        archived.archived_at = Some("2026-01-01".into());
        assert!(is_gone(&archived));

        let mut trashed = issue("ENG-2", "x");
        trashed.trashed = Some(true);
        assert!(is_gone(&trashed));

        assert!(!is_gone(&issue("ENG-3", "x")));
    }

    // ── reconcile_deletions ─────────────────────────────────────────────

    #[test]
    fn reconcile_prunes_everything_not_active() {
        let mut stored = HashMap::new();
        stored.insert("linear://ENG-1".to_string(), "h1".to_string()); // active
        stored.insert("linear://ENG-2".to_string(), "h2".to_string()); // aged out / gone
        stored.insert("linear://ENG-3".to_string(), "h3".to_string()); // gone
        let mut active = HashSet::new();
        active.insert("linear://ENG-1".to_string());

        let mut deletions = reconcile_deletions(&stored, &active);
        deletions.sort();
        assert_eq!(deletions, vec!["linear://ENG-2", "linear://ENG-3"]);
    }

    #[test]
    fn reconcile_keeps_active_even_when_hash_unchanged() {
        // Active issues are retained regardless of hash-skip.
        let mut stored = HashMap::new();
        stored.insert("linear://ENG-1".to_string(), "h1".to_string());
        let mut active = HashSet::new();
        active.insert("linear://ENG-1".to_string());
        assert!(reconcile_deletions(&stored, &active).is_empty());
    }

    // ── fetch orchestration via a mock fetcher ──────────────────────────

    struct MockFetcher {
        pages: Vec<IssueConnection>,
        calls: std::sync::Mutex<usize>,
    }

    impl MockFetcher {
        fn new(pages: Vec<IssueConnection>) -> Self {
            Self {
                pages,
                calls: std::sync::Mutex::new(0),
            }
        }
    }

    #[async_trait]
    impl IssueFetcher for MockFetcher {
        async fn fetch_page(
            &self,
            _first: u32,
            _after: Option<String>,
            _order_by: &str,
            _team: Option<&str>,
        ) -> Result<IssueConnection> {
            let mut calls = self.calls.lock().unwrap();
            let page = self.pages.get(*calls).cloned().unwrap_or_default();
            *calls += 1;
            Ok(page)
        }
    }

    fn page(issues: Vec<Issue>, has_next: bool, cursor: Option<&str>) -> IssueConnection {
        IssueConnection {
            nodes: issues,
            page_info: PageInfo {
                has_next_page: has_next,
                end_cursor: cursor.map(String::from),
            },
        }
    }

    fn ctx(full: bool, stored: HashMap<String, String>) -> FetchContext {
        FetchContext {
            full,
            cursor: None,
            stored_hashes: stored,
        }
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn fetch_paginates_until_amount() {
        let settings = LinearTapSettings {
            amount: 3,
            ..LinearTapSettings::default()
        };
        let fetcher = MockFetcher::new(vec![
            page(
                vec![issue("ENG-1", "a"), issue("ENG-2", "b")],
                true,
                Some("c1"),
            ),
            page(
                vec![issue("ENG-3", "c"), issue("ENG-4", "d")],
                true,
                Some("c2"),
            ),
        ]);
        let tap = LinearTap::with_fetcher(settings, Box::new(fetcher));
        let result = tap.fetch(&ctx(true, HashMap::new())).await.unwrap();
        // amount = 3 → truncated across two pages.
        assert_eq!(result.items.len(), 3);
        let paths: Vec<&str> = result
            .items
            .iter()
            .map(|i| i.source_path.as_str())
            .collect();
        assert!(paths.contains(&"linear://ENG-1"));
        assert!(paths.contains(&"linear://ENG-3"));
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn fetch_stops_when_no_next_page() {
        let settings = LinearTapSettings {
            amount: 100,
            ..LinearTapSettings::default()
        };
        let fetcher = MockFetcher::new(vec![page(vec![issue("ENG-1", "a")], false, None)]);
        let tap = LinearTap::with_fetcher(settings, Box::new(fetcher));
        let result = tap.fetch(&ctx(true, HashMap::new())).await.unwrap();
        assert_eq!(result.items.len(), 1);
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn fetch_skips_unchanged_but_does_not_delete_them() {
        let settings = LinearTapSettings {
            amount: 10,
            ..LinearTapSettings::default()
        };
        // Pre-compute ENG-1's hash so we can mark it unchanged.
        let unchanged = issue_to_source_item(&issue("ENG-1", "a"), &settings);
        let mut stored = HashMap::new();
        stored.insert(
            unchanged.source_path.clone(),
            unchanged.content_hash.clone(),
        );

        let fetcher = MockFetcher::new(vec![page(
            vec![issue("ENG-1", "a"), issue("ENG-2", "b")],
            false,
            None,
        )]);
        let tap = LinearTap::with_fetcher(settings, Box::new(fetcher));
        let result = tap.fetch(&ctx(false, stored)).await.unwrap();

        // ENG-1 skipped (unchanged), ENG-2 processed.
        let paths: Vec<&str> = result
            .items
            .iter()
            .map(|i| i.source_path.as_str())
            .collect();
        assert!(!paths.contains(&"linear://ENG-1"));
        assert!(paths.contains(&"linear://ENG-2"));
        // ENG-1 still active → not deleted.
        assert!(result.deletions.is_empty());
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn fetch_prunes_archived_and_aged_out() {
        let settings = LinearTapSettings {
            amount: 10,
            ..LinearTapSettings::default()
        };
        let mut archived = issue("ENG-2", "old");
        archived.archived_at = Some("2026-01-01".into());

        let mut stored = HashMap::new();
        stored.insert("linear://ENG-1".to_string(), "stale".to_string()); // will stay (active, changed)
        stored.insert("linear://ENG-2".to_string(), "h2".to_string()); // archived → delete
        stored.insert("linear://ENG-9".to_string(), "h9".to_string()); // aged out → delete

        let fetcher =
            MockFetcher::new(vec![page(vec![issue("ENG-1", "a"), archived], false, None)]);
        let tap = LinearTap::with_fetcher(settings, Box::new(fetcher));
        let result = tap.fetch(&ctx(false, stored)).await.unwrap();

        let mut deletions = result.deletions.clone();
        deletions.sort();
        assert_eq!(deletions, vec!["linear://ENG-2", "linear://ENG-9"]);
        // Archived issue is never indexed.
        let paths: Vec<&str> = result
            .items
            .iter()
            .map(|i| i.source_path.as_str())
            .collect();
        assert!(!paths.contains(&"linear://ENG-2"));
        assert!(paths.contains(&"linear://ENG-1"));
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn fetch_cursor_is_max_updated_at() {
        let mut a = issue("ENG-1", "a");
        a.updated_at = Some("2026-01-01T00:00:00Z".into());
        let mut b = issue("ENG-2", "b");
        b.updated_at = Some("2026-03-01T00:00:00Z".into());
        let fetcher = MockFetcher::new(vec![page(vec![a, b], false, None)]);
        let tap = LinearTap::with_fetcher(LinearTapSettings::default(), Box::new(fetcher));
        let result = tap.fetch(&ctx(true, HashMap::new())).await.unwrap();
        assert_eq!(result.cursor.as_deref(), Some("2026-03-01T00:00:00Z"));
    }
}
