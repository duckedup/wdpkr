//! Built-in Notion tap: indexes specific Notion pages ("implementation specs")
//! on demand.
//!
//! Unlike the Linear tap — which syncs the newest N issues and reconciles the
//! whole set — the Notion tap is **targeted and additive**. It fetches only the
//! page IDs passed via `wdpkr index --tap notion --doc <id-or-url>` and never
//! prunes other documents. Removal is explicit (`wdpkr delete --tap notion`).
//!
//! Each page becomes one document keyed `notion://{page_id}`: a doc-level
//! summary vector, plus one **section child** per heading (split on
//! `heading_1/2/3`) when the page has headings. That lets an agent match the
//! exact section and use the page ID to fetch the full document for context.
//!
//! Sub-pages (`child_page` blocks) are **not** crawled — they're recorded as
//! link references in the parent's text. The caller indexes sub-pages
//! explicitly with their own `--doc`.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::future::Future;
use std::pin::Pin;

use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use serde_json::Value;

use super::{FetchContext, FetchResult, SourceChunk, SourceItem, Tap};
use crate::http::{self, RetryPolicy};

const DEFAULT_API_KEY_ENV: &str = "NOTION_API_KEY";
/// Pinned Notion API version (see `Notion-Version` header). Overridable via
/// `settings.notion_version`.
const DEFAULT_NOTION_VERSION: &str = "2026-03-11";
const DEFAULT_BASE_URL: &str = "https://api.notion.com";
/// Notion caps block-children pages at 100.
const PAGE_SIZE: u32 = 100;
const MAX_RETRIES: usize = 3;
/// URI scheme used for a Notion page's `source_path`.
pub const SOURCE_SCHEME: &str = "notion://";

// ── Settings ─────────────────────────────────────────────────────────────────

/// Typed view of the `notion` tap's `settings` map. Unknown keys (e.g. the
/// per-tap `decay` block consumed by search) are ignored.
#[derive(Debug, Clone)]
pub struct NotionTapSettings {
    /// Inline integration token from `settings.api_key`. Takes precedence over
    /// `api_key_env` when set. Convenient for local config; keep the file private.
    pub api_key: Option<String>,
    /// Name of the env var holding the Notion integration token (fallback when
    /// `api_key` is unset).
    pub api_key_env: String,
    /// `Notion-Version` header value.
    pub notion_version: String,
    /// Emit per-heading section children in addition to the doc-level summary.
    pub include_sections: bool,
}

impl Default for NotionTapSettings {
    fn default() -> Self {
        Self {
            api_key: None,
            api_key_env: DEFAULT_API_KEY_ENV.to_string(),
            notion_version: DEFAULT_NOTION_VERSION.to_string(),
            include_sections: true,
        }
    }
}

impl NotionTapSettings {
    pub fn from_settings(settings: &HashMap<String, serde_yaml::Value>) -> Result<Self> {
        let mut out = Self::default();
        if let Some(v) = settings.get("api_key") {
            out.api_key = Some(
                v.as_str()
                    .ok_or_else(|| anyhow!("notion tap: 'api_key' must be a string"))?
                    .to_string(),
            );
        }
        if let Some(v) = settings.get("api_key_env") {
            out.api_key_env = v
                .as_str()
                .ok_or_else(|| anyhow!("notion tap: 'api_key_env' must be a string"))?
                .to_string();
        }
        if let Some(v) = settings.get("notion_version") {
            out.notion_version = v
                .as_str()
                .ok_or_else(|| anyhow!("notion tap: 'notion_version' must be a string"))?
                .to_string();
        }
        if let Some(v) = settings.get("include_sections") {
            out.include_sections = v
                .as_bool()
                .ok_or_else(|| anyhow!("notion tap: 'include_sections' must be a boolean"))?;
        }
        Ok(out)
    }
}

// ── Parsed Notion types (wdpkr-owned) ─────────────────────────────────────────

/// Page metadata.
#[derive(Debug, Clone)]
struct PageMeta {
    title: String,
    url: Option<String>,
}

/// A single parsed block, flattened to the text we care about.
#[derive(Debug, Clone)]
struct Block {
    id: String,
    /// Notion block type: `paragraph`, `heading_1`, `code`, `child_page`, ...
    kind: String,
    /// Concatenated `plain_text` of the block's `rich_text` (empty for blocks
    /// without rich text, e.g. `child_page`).
    text: String,
    /// `code` block language.
    language: Option<String>,
    /// `child_page` title.
    child_page_title: Option<String>,
    /// Whether the block has nested children to fetch.
    has_children: bool,
}

/// One page of a block's children from the API.
#[derive(Debug, Clone)]
struct BlockPage {
    blocks: Vec<Block>,
    next_cursor: Option<String>,
}

// ── Fetching (mockable) ───────────────────────────────────────────────────────

/// Notion read operations. Abstracted so the tap's orchestration (pagination,
/// nested-block recursion, markdown assembly, hash-skip) is unit-testable with
/// no HTTP.
#[async_trait]
trait PageFetcher: Send + Sync {
    async fn fetch_meta(&self, page_id: &str) -> Result<PageMeta>;
    async fn fetch_children(&self, block_id: &str, cursor: Option<String>) -> Result<BlockPage>;
}

/// Real fetcher backed by the Notion REST API over the shared retry client.
struct NotionApiFetcher {
    client: reqwest::Client,
    api_key: String,
    notion_version: String,
    base_url: String,
}

#[async_trait]
impl PageFetcher for NotionApiFetcher {
    async fn fetch_meta(&self, page_id: &str) -> Result<PageMeta> {
        let url = format!("{}/v1/pages/{}", self.base_url, page_id);
        let policy = RetryPolicy::standard(MAX_RETRIES, 1000);
        let resp = http::send_with_retry(&policy, "Notion pages API", || {
            self.client
                .get(&url)
                .bearer_auth(&self.api_key)
                .header("Notion-Version", &self.notion_version)
        })
        .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Notion page {page_id} fetch failed ({status}): {body}");
        }
        let v: Value = resp.json().await?;
        Ok(parse_page_meta(&v))
    }

    async fn fetch_children(&self, block_id: &str, cursor: Option<String>) -> Result<BlockPage> {
        let mut url = format!(
            "{}/v1/blocks/{}/children?page_size={}",
            self.base_url, block_id, PAGE_SIZE
        );
        if let Some(ref c) = cursor {
            let _ = write!(url, "&start_cursor={c}");
        }
        let policy = RetryPolicy::standard(MAX_RETRIES, 1000);
        let resp = http::send_with_retry(&policy, "Notion blocks API", || {
            self.client
                .get(&url)
                .bearer_auth(&self.api_key)
                .header("Notion-Version", &self.notion_version)
        })
        .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Notion blocks fetch failed for {block_id} ({status}): {body}");
        }
        let v: Value = resp.json().await?;
        Ok(parse_block_page(&v))
    }
}

// ── Tap ────────────────────────────────────────────────────────────────────────

pub struct NotionTap {
    settings: NotionTapSettings,
    /// Page IDs / URLs to index this run (from `--doc`).
    targets: Vec<String>,
    fetcher: Box<dyn PageFetcher>,
}

impl NotionTap {
    /// Build from the tap's `settings` map and the run's `--doc` targets,
    /// resolving the API token from the configured env var.
    pub fn from_settings(
        settings: &HashMap<String, serde_yaml::Value>,
        targets: Vec<String>,
    ) -> Result<Self> {
        let settings = NotionTapSettings::from_settings(settings)?;
        let api_key = match settings.api_key.as_deref() {
            Some(k) if !k.is_empty() => k.to_string(),
            _ => std::env::var(&settings.api_key_env).map_err(|_| {
                anyhow!(
                    "Notion API key not set: put it in the notion tap's settings.api_key, \
                     or export {}",
                    settings.api_key_env
                )
            })?,
        };
        let fetcher = NotionApiFetcher {
            client: reqwest::Client::new(),
            api_key,
            notion_version: settings.notion_version.clone(),
            base_url: DEFAULT_BASE_URL.to_string(),
        };
        Ok(Self {
            settings,
            targets,
            fetcher: Box::new(fetcher),
        })
    }

    #[cfg(test)]
    fn with_fetcher(
        settings: NotionTapSettings,
        targets: Vec<String>,
        fetcher: Box<dyn PageFetcher>,
    ) -> Self {
        Self {
            settings,
            targets,
            fetcher,
        }
    }

    /// Collect all blocks of a page in document order, inlining nested
    /// non-`child_page` children depth-first. `child_page` blocks are kept as
    /// a single link-reference block and never crawled.
    fn collect_blocks<'a>(
        &'a self,
        block_id: &'a str,
        out: &'a mut Vec<Block>,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let mut cursor: Option<String> = None;
            loop {
                let page = self.fetcher.fetch_children(block_id, cursor).await?;
                for block in page.blocks {
                    let recurse = block.has_children && block.kind != "child_page";
                    let child_id = block.id.clone();
                    out.push(block);
                    if recurse {
                        self.collect_blocks(&child_id, out).await?;
                    }
                }
                match page.next_cursor {
                    Some(c) => cursor = Some(c),
                    None => break,
                }
            }
            Ok(())
        })
    }

    async fn fetch_page_item(&self, target: &str) -> Result<SourceItem> {
        let page_id = parse_page_id(target)?;
        let meta = self.fetcher.fetch_meta(&page_id).await?;
        let mut blocks = Vec::new();
        self.collect_blocks(&page_id, &mut blocks).await?;
        Ok(build_source_item(
            &page_id,
            &meta,
            &blocks,
            self.settings.include_sections,
        ))
    }
}

#[async_trait]
impl Tap for NotionTap {
    fn name(&self) -> &str {
        "notion"
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<FetchResult> {
        let mut items = Vec::new();
        for target in &self.targets {
            let item = self.fetch_page_item(target).await?;
            // Skip unchanged docs on incremental runs (still no deletions —
            // Notion indexing is additive).
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
        Ok(FetchResult {
            items,
            deletions: Vec::new(),
            cursor: None,
        })
    }
}

// ── Pure helpers (no HTTP, Miri-friendly) ─────────────────────────────────────

/// Extract a Notion page ID from a URL or bare ID and normalize it to a dashed
/// lowercase UUID (`8-4-4-4-12`). Accepts:
/// - a bare 32-hex ID or a dashed UUID,
/// - a Notion URL whose final path segment ends in the 32-hex ID
///   (e.g. `https://app.notion.com/p/Implementation-Specs-399cb3ca…`).
pub fn parse_page_id(input: &str) -> Result<String> {
    let trimmed = input.trim();
    // Drop query string / fragment.
    let no_query = trimmed.split(['?', '#']).next().unwrap_or(trimmed);
    // Take the last path segment (works for bare IDs too — no '/').
    let segment = no_query.rsplit('/').next().unwrap_or(no_query);
    // Notion slugs are `Title-<32hex>`; a bare dashed UUID is `8-4-4-4-12`.
    // Removing dashes and taking the trailing 32 hex chars handles both.
    let compact: String = segment.chars().filter(|c| *c != '-').collect();
    if compact.len() < 32 {
        bail!("could not extract a Notion page id from '{input}'");
    }
    let id: String = compact[compact.len() - 32..].to_ascii_lowercase();
    if !id.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("could not extract a Notion page id from '{input}' (not a 32-hex id)");
    }
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &id[0..8],
        &id[8..12],
        &id[12..16],
        &id[16..20],
        &id[20..32]
    ))
}

/// Join the `plain_text` fields of a `rich_text` array.
fn rich_text_plain(rich_text: &Value) -> String {
    rich_text
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|rt| rt.get("plain_text").and_then(Value::as_str))
                .collect::<String>()
        })
        .unwrap_or_default()
}

/// Parse the `properties` of a page object into title + url.
fn parse_page_meta(v: &Value) -> PageMeta {
    let url = v.get("url").and_then(Value::as_str).map(String::from);
    // The title lives in whichever property has `type == "title"`.
    let title = v
        .get("properties")
        .and_then(Value::as_object)
        .and_then(|props| {
            props.values().find_map(|prop| {
                if prop.get("type").and_then(Value::as_str) == Some("title") {
                    Some(rich_text_plain(prop.get("title")?))
                } else {
                    None
                }
            })
        })
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| "Untitled".to_string());
    PageMeta { title, url }
}

/// Parse one `GET /blocks/{id}/children` response into a [`BlockPage`].
fn parse_block_page(v: &Value) -> BlockPage {
    let blocks = v
        .get("results")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(parse_block).collect())
        .unwrap_or_default();
    let next_cursor = if v.get("has_more").and_then(Value::as_bool) == Some(true) {
        v.get("next_cursor")
            .and_then(Value::as_str)
            .map(String::from)
    } else {
        None
    };
    BlockPage {
        blocks,
        next_cursor,
    }
}

/// Parse a single block object into our flattened [`Block`].
fn parse_block(v: &Value) -> Option<Block> {
    let kind = v.get("type").and_then(Value::as_str)?.to_string();
    let id = v.get("id").and_then(Value::as_str)?.to_string();
    let inner = v.get(&kind);
    let text = inner
        .and_then(|o| o.get("rich_text"))
        .map(rich_text_plain)
        .unwrap_or_default();
    let language = inner
        .and_then(|o| o.get("language"))
        .and_then(Value::as_str)
        .map(String::from);
    let child_page_title = if kind == "child_page" {
        inner
            .and_then(|o| o.get("title"))
            .and_then(Value::as_str)
            .map(String::from)
    } else {
        None
    };
    let has_children = v
        .get("has_children")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Some(Block {
        id,
        kind,
        text,
        language,
        child_page_title,
        has_children,
    })
}

/// Render a single block to a markdown line (no trailing newline).
fn render_block(block: &Block) -> Option<String> {
    match block.kind.as_str() {
        "heading_1" | "heading_2" | "heading_3" => None, // headings become section boundaries
        "child_page" => Some(format!(
            "[sub-page: {}]",
            block.child_page_title.as_deref().unwrap_or("untitled")
        )),
        "code" => {
            let lang = block.language.as_deref().unwrap_or("");
            Some(format!("```{lang}\n{}\n```", block.text))
        }
        "bulleted_list_item" | "to_do" | "toggle" => Some(format!("- {}", block.text)),
        "numbered_list_item" => Some(format!("1. {}", block.text)),
        "quote" | "callout" => Some(format!("> {}", block.text)),
        _ => {
            if block.text.is_empty() {
                None
            } else {
                Some(block.text.clone())
            }
        }
    }
}

/// A heading-delimited section of a page.
struct Section {
    name: String,
    body: String,
}

/// Split a page's blocks into sections on `heading_1/2/3`. Content before the
/// first heading becomes an "Overview" section. Returns `(sections, has_headings)`.
fn split_sections(title: &str, blocks: &[Block]) -> (Vec<Section>, bool) {
    let mut sections: Vec<Section> = Vec::new();
    let mut has_headings = false;
    let mut current = Section {
        name: format!("{title} — Overview"),
        body: String::new(),
    };
    for block in blocks {
        if matches!(block.kind.as_str(), "heading_1" | "heading_2" | "heading_3") {
            has_headings = true;
            if !current.body.trim().is_empty() {
                sections.push(current);
            }
            current = Section {
                name: block.text.trim().to_string(),
                body: String::new(),
            };
        } else if let Some(line) = render_block(block) {
            current.body.push_str(&line);
            current.body.push('\n');
        }
    }
    if !current.body.trim().is_empty() {
        sections.push(current);
    }
    (sections, has_headings)
}

/// Build the doc-level markdown content that gets summarized + embedded.
fn render_document(meta: &PageMeta, blocks: &[Block]) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "# {}", meta.title);
    if let Some(url) = meta.url.as_deref().filter(|u| !u.is_empty()) {
        let _ = writeln!(s, "URL: {url}");
    }
    s.push('\n');
    for block in blocks {
        match block.kind.as_str() {
            "heading_1" | "heading_2" | "heading_3" => {
                let _ = writeln!(s, "\n## {}\n", block.text.trim());
            }
            _ => {
                if let Some(line) = render_block(block) {
                    let _ = writeln!(s, "{line}");
                }
            }
        }
    }
    s
}

/// Build a [`SourceItem`] for a Notion page: doc-level content keyed
/// `notion://{page_id}`, with one section child per heading when the page has
/// structure (and `include_sections` is on).
fn build_source_item(
    page_id: &str,
    meta: &PageMeta,
    blocks: &[Block],
    include_sections: bool,
) -> SourceItem {
    let content = render_document(meta, blocks);
    let content_hash = blake3::hash(content.as_bytes()).to_hex()[..16].to_string();

    let children = if include_sections {
        let (sections, has_headings) = split_sections(&meta.title, blocks);
        // Only emit section children when the page actually has headings —
        // otherwise the single "Overview" section would just duplicate the
        // doc-level vector.
        if has_headings {
            sections
                .into_iter()
                .map(|sec| SourceChunk {
                    name: sec.name,
                    kind: "section".to_string(),
                    content: sec.body.trim().to_string(),
                    signature: None,
                    doc_comment: None,
                    start_line: None,
                    end_line: None,
                    references: Vec::new(),
                })
                .collect()
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };

    SourceItem {
        source_path: format!("{SOURCE_SCHEME}{page_id}"),
        content,
        content_hash,
        language: None,
        module_doc: None,
        children,
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── settings ──────────────────────────────────────────────────────

    #[test]
    fn settings_defaults() {
        let s = NotionTapSettings::from_settings(&HashMap::new()).unwrap();
        assert!(s.api_key.is_none());
        assert_eq!(s.api_key_env, "NOTION_API_KEY");
        assert_eq!(s.notion_version, DEFAULT_NOTION_VERSION);
        assert!(s.include_sections);
    }

    #[test]
    fn settings_parses_inline_api_key() {
        let mut m = HashMap::new();
        m.insert(
            "api_key".into(),
            serde_yaml::Value::String("secret_abc".into()),
        );
        let s = NotionTapSettings::from_settings(&m).unwrap();
        assert_eq!(s.api_key.as_deref(), Some("secret_abc"));
    }

    #[test]
    fn settings_overrides_and_ignores_unknown() {
        let mut m = HashMap::new();
        m.insert(
            "api_key_env".into(),
            serde_yaml::Value::String("MY_NOTION".into()),
        );
        m.insert("include_sections".into(), serde_yaml::Value::Bool(false));
        // A `decay` block (consumed elsewhere) must be ignored, not rejected.
        m.insert("decay".into(), serde_yaml::Value::String("whatever".into()));
        let s = NotionTapSettings::from_settings(&m).unwrap();
        assert_eq!(s.api_key_env, "MY_NOTION");
        assert!(!s.include_sections);
    }

    #[test]
    fn settings_rejects_wrong_type() {
        let mut m = HashMap::new();
        m.insert(
            "include_sections".into(),
            serde_yaml::Value::String("yes".into()),
        );
        let err = NotionTapSettings::from_settings(&m).unwrap_err();
        assert!(err.to_string().contains("include_sections"), "got: {err}");
    }

    // ── parse_page_id ─────────────────────────────────────────────────

    #[test]
    fn parse_id_from_app_url_with_slug() {
        let id = parse_page_id(
            "https://app.notion.com/p/Implementation-Specs-399cb3cadc9580a1a77fd58e1248ded6?source=copy_link",
        )
        .unwrap();
        assert_eq!(id, "399cb3ca-dc95-80a1-a77f-d58e1248ded6");
    }

    #[test]
    fn parse_id_from_notion_so_url() {
        let id = parse_page_id("https://www.notion.so/My-Page-399cb3cadc9580a1a77fd58e1248ded6")
            .unwrap();
        assert_eq!(id, "399cb3ca-dc95-80a1-a77f-d58e1248ded6");
    }

    #[test]
    fn parse_id_from_bare_hex() {
        let id = parse_page_id("399cb3cadc9580a1a77fd58e1248ded6").unwrap();
        assert_eq!(id, "399cb3ca-dc95-80a1-a77f-d58e1248ded6");
    }

    #[test]
    fn parse_id_from_dashed_uuid() {
        let id = parse_page_id("399cb3ca-dc95-80a1-a77f-d58e1248ded6").unwrap();
        assert_eq!(id, "399cb3ca-dc95-80a1-a77f-d58e1248ded6");
    }

    #[test]
    fn parse_id_rejects_garbage() {
        assert!(parse_page_id("not-a-notion-link").is_err());
        assert!(parse_page_id("https://app.notion.com/p/short").is_err());
    }

    // ── JSON parsing ──────────────────────────────────────────────────

    #[test]
    fn parse_page_meta_extracts_title_and_url() {
        let v = serde_json::json!({
            "object": "page",
            "id": "399cb3ca-dc95-80a1-a77f-d58e1248ded6",
            "url": "https://notion.so/Implementation-Specs-399cb3ca",
            "properties": {
                "Name": {
                    "id": "title",
                    "type": "title",
                    "title": [{ "plain_text": "Implementation Specs" }]
                }
            }
        });
        let meta = parse_page_meta(&v);
        assert_eq!(meta.title, "Implementation Specs");
        assert_eq!(
            meta.url.as_deref(),
            Some("https://notion.so/Implementation-Specs-399cb3ca")
        );
    }

    #[test]
    fn parse_page_meta_untitled_fallback() {
        let v = serde_json::json!({ "object": "page", "properties": {} });
        assert_eq!(parse_page_meta(&v).title, "Untitled");
    }

    #[test]
    fn parse_block_page_reads_blocks_and_cursor() {
        let v = serde_json::json!({
            "results": [
                {
                    "object": "block",
                    "id": "b1",
                    "type": "heading_1",
                    "heading_1": { "rich_text": [{ "plain_text": "Overview" }] },
                    "has_children": false
                },
                {
                    "object": "block",
                    "id": "b2",
                    "type": "paragraph",
                    "paragraph": { "rich_text": [{ "plain_text": "Hello " }, { "plain_text": "world" }] },
                    "has_children": false
                }
            ],
            "has_more": true,
            "next_cursor": "cursor-2"
        });
        let page = parse_block_page(&v);
        assert_eq!(page.blocks.len(), 2);
        assert_eq!(page.blocks[0].kind, "heading_1");
        assert_eq!(page.blocks[0].text, "Overview");
        assert_eq!(page.blocks[1].text, "Hello world");
        assert_eq!(page.next_cursor.as_deref(), Some("cursor-2"));
    }

    #[test]
    fn parse_block_page_no_more_has_no_cursor() {
        let v = serde_json::json!({ "results": [], "has_more": false, "next_cursor": null });
        assert!(parse_block_page(&v).next_cursor.is_none());
    }

    #[test]
    fn parse_block_reads_child_page_title() {
        let v = serde_json::json!({
            "id": "cp1",
            "type": "child_page",
            "child_page": { "title": "API Design" },
            "has_children": true
        });
        let b = parse_block(&v).unwrap();
        assert_eq!(b.kind, "child_page");
        assert_eq!(b.child_page_title.as_deref(), Some("API Design"));
    }

    // ── document / section building ───────────────────────────────────

    fn block(kind: &str, text: &str) -> Block {
        Block {
            id: format!("id-{kind}-{text}"),
            kind: kind.into(),
            text: text.into(),
            language: None,
            child_page_title: None,
            has_children: false,
        }
    }

    fn meta() -> PageMeta {
        PageMeta {
            title: "Commission Spec".into(),
            url: Some("https://notion.so/x".into()),
        }
    }

    #[test]
    fn document_includes_title_and_headings() {
        let blocks = vec![
            block("paragraph", "Intro text."),
            block("heading_2", "Rounding rules"),
            block("paragraph", "Round half up."),
        ];
        let doc = render_document(&meta(), &blocks);
        assert!(doc.contains("# Commission Spec"));
        assert!(doc.contains("## Rounding rules"));
        assert!(doc.contains("Round half up."));
        assert!(doc.contains("Intro text."));
    }

    #[test]
    fn sections_split_on_headings_with_overview() {
        let blocks = vec![
            block("paragraph", "Intro text."),
            block("heading_2", "Rounding rules"),
            block("paragraph", "Round half up."),
            block("heading_2", "Edge cases"),
            block("paragraph", "Zero payout."),
        ];
        let (sections, has_headings) = split_sections("Commission Spec", &blocks);
        assert!(has_headings);
        assert_eq!(sections.len(), 3); // overview + 2 headings
        assert!(sections[0].name.contains("Overview"));
        assert_eq!(sections[1].name, "Rounding rules");
        assert!(sections[1].body.contains("Round half up."));
        assert_eq!(sections[2].name, "Edge cases");
    }

    #[test]
    fn build_item_keys_on_page_id_with_section_children() {
        let blocks = vec![
            block("paragraph", "Intro."),
            block("heading_1", "Design"),
            block("paragraph", "Details."),
        ];
        let item = build_source_item(
            "399cb3ca-dc95-80a1-a77f-d58e1248ded6",
            &meta(),
            &blocks,
            true,
        );
        assert_eq!(
            item.source_path,
            "notion://399cb3ca-dc95-80a1-a77f-d58e1248ded6"
        );
        assert!(item.language.is_none());
        assert!(!item.content_hash.is_empty());
        assert!(!item.children.is_empty());
        assert!(item.children.iter().all(|c| c.kind == "section"));
    }

    #[test]
    fn build_item_no_headings_has_no_children() {
        let blocks = vec![
            block("paragraph", "Just a flat note."),
            block("paragraph", "More."),
        ];
        let item = build_source_item(
            "399cb3ca-dc95-80a1-a77f-d58e1248ded6",
            &meta(),
            &blocks,
            true,
        );
        assert!(
            item.children.is_empty(),
            "no headings → no section children (avoids duplicate vector)"
        );
    }

    #[test]
    fn build_item_include_sections_false_has_no_children() {
        let blocks = vec![block("heading_1", "H"), block("paragraph", "text")];
        let item = build_source_item(
            "399cb3ca-dc95-80a1-a77f-d58e1248ded6",
            &meta(),
            &blocks,
            false,
        );
        assert!(item.children.is_empty());
    }

    #[test]
    fn child_page_rendered_as_link_not_crawled() {
        let mut cp = block("child_page", "");
        cp.child_page_title = Some("API Design".into());
        let doc = render_document(&meta(), &[cp]);
        assert!(doc.contains("[sub-page: API Design]"));
    }

    #[test]
    fn content_hash_is_content_sensitive() {
        let a = build_source_item(
            "id0000000000000000000000000000aa",
            &meta(),
            &[block("paragraph", "x")],
            true,
        );
        let b = build_source_item(
            "id0000000000000000000000000000aa",
            &meta(),
            &[block("paragraph", "y")],
            true,
        );
        assert_ne!(a.content_hash, b.content_hash);
    }

    // ── fetch orchestration via a mock fetcher ────────────────────────

    struct MockFetcher {
        meta: PageMeta,
        /// Maps block_id → the sequence of pages to return. Pagination is
        /// cursor-driven (cursor = the next page index as a string), so the
        /// mock is idempotent across repeated `fetch()` calls.
        pages: HashMap<String, Vec<BlockPage>>,
    }

    #[async_trait]
    impl PageFetcher for MockFetcher {
        async fn fetch_meta(&self, _page_id: &str) -> Result<PageMeta> {
            Ok(self.meta.clone())
        }
        async fn fetch_children(
            &self,
            block_id: &str,
            cursor: Option<String>,
        ) -> Result<BlockPage> {
            let idx: usize = cursor.as_deref().and_then(|c| c.parse().ok()).unwrap_or(0);
            let page = self
                .pages
                .get(block_id)
                .and_then(|v| v.get(idx))
                .cloned()
                .unwrap_or(BlockPage {
                    blocks: vec![],
                    next_cursor: None,
                });
            Ok(page)
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
    async fn fetch_builds_item_for_target() {
        let page_id = "399cb3ca-dc95-80a1-a77f-d58e1248ded6";
        let mut pages = HashMap::new();
        pages.insert(
            page_id.to_string(),
            vec![BlockPage {
                blocks: vec![block("heading_1", "Design"), block("paragraph", "Details.")],
                next_cursor: None,
            }],
        );
        let fetcher = MockFetcher {
            meta: meta(),
            pages,
        };
        let tap = NotionTap::with_fetcher(
            NotionTapSettings::default(),
            vec![format!(
                "https://app.notion.com/p/Spec-{}",
                page_id.replace('-', "")
            )],
            Box::new(fetcher),
        );
        let result = tap.fetch(&ctx(true, HashMap::new())).await.unwrap();
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].source_path, format!("notion://{page_id}"));
        assert!(result.deletions.is_empty(), "notion indexing is additive");
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn fetch_recurses_nested_non_child_page_blocks() {
        let page_id = "399cb3ca-dc95-80a1-a77f-d58e1248ded6";
        let mut toggle = block("toggle", "Toggle header");
        toggle.has_children = true;
        toggle.id = "toggle-1".into();
        let mut pages = HashMap::new();
        pages.insert(
            page_id.to_string(),
            vec![BlockPage {
                blocks: vec![toggle],
                next_cursor: None,
            }],
        );
        pages.insert(
            "toggle-1".to_string(),
            vec![BlockPage {
                blocks: vec![block("paragraph", "Nested detail.")],
                next_cursor: None,
            }],
        );
        let fetcher = MockFetcher {
            meta: meta(),
            pages,
        };
        let tap = NotionTap::with_fetcher(
            NotionTapSettings::default(),
            vec![page_id.to_string()],
            Box::new(fetcher),
        );
        let result = tap.fetch(&ctx(true, HashMap::new())).await.unwrap();
        assert!(result.items[0].content.contains("Nested detail."));
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn fetch_does_not_crawl_child_pages() {
        let page_id = "399cb3ca-dc95-80a1-a77f-d58e1248ded6";
        let mut cp = block("child_page", "");
        cp.child_page_title = Some("Sub Spec".into());
        cp.has_children = true;
        cp.id = "child-1".into();
        let mut pages = HashMap::new();
        pages.insert(
            page_id.to_string(),
            vec![BlockPage {
                blocks: vec![cp],
                next_cursor: None,
            }],
        );
        // If crawling happened, this would be pulled in — assert it is NOT.
        pages.insert(
            "child-1".to_string(),
            vec![BlockPage {
                blocks: vec![block("paragraph", "SHOULD NOT APPEAR")],
                next_cursor: None,
            }],
        );
        let fetcher = MockFetcher {
            meta: meta(),
            pages,
        };
        let tap = NotionTap::with_fetcher(
            NotionTapSettings::default(),
            vec![page_id.to_string()],
            Box::new(fetcher),
        );
        let result = tap.fetch(&ctx(true, HashMap::new())).await.unwrap();
        assert!(!result.items[0].content.contains("SHOULD NOT APPEAR"));
        assert!(result.items[0].content.contains("[sub-page: Sub Spec]"));
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn fetch_skips_unchanged_on_incremental() {
        let page_id = "399cb3ca-dc95-80a1-a77f-d58e1248ded6";
        let mut pages = HashMap::new();
        pages.insert(
            page_id.to_string(),
            vec![BlockPage {
                blocks: vec![block("paragraph", "stable")],
                next_cursor: None,
            }],
        );
        let fetcher = MockFetcher {
            meta: meta(),
            pages,
        };
        let tap = NotionTap::with_fetcher(
            NotionTapSettings::default(),
            vec![page_id.to_string()],
            Box::new(fetcher),
        );
        // First compute the hash the tap would produce.
        let first = tap.fetch(&ctx(true, HashMap::new())).await.unwrap();
        let hash = first.items[0].content_hash.clone();
        let mut stored = HashMap::new();
        stored.insert(format!("notion://{page_id}"), hash);
        let second = tap.fetch(&ctx(false, stored)).await.unwrap();
        assert!(second.items.is_empty(), "unchanged doc should be skipped");
    }
}
