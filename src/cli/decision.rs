//! `wdpkr decision` — author and manage store-native architectural decisions.
//!
//! Decisions are not files on disk: they are embedded into the `<base>--decision`
//! namespace of the configured store, with their structured metadata kept in a
//! JSON registry inside that namespace's `NamespaceMetadata.extra`. See
//! [`crate::decision`] for the pure data model.

use std::collections::HashMap;

use anyhow::{Result, anyhow, bail};
use clap::{Args, Subcommand};
use owo_colors::{OwoColorize, Stream};

use crate::config::Config;
use crate::decision::{
    DecisionEntry, DecisionRegistry, DecisionStatus, REGISTRY_META_KEY, SourceRef, TAP_NAME,
};
use crate::embed::{Embedder, build_embedder, embedder_identity};
use crate::indexer::pipeline::{EmbedMode, process_item};
use crate::indexer::{now_unix_secs, resolve_namespace};
use crate::store::{Namespace, NamespaceMetadata, VectorStore, build_store};
use crate::tap::{FetchContext, Tap, build_tap, namespace_suffix};

// ── Args ─────────────────────────────────────────────────────────────────────

#[derive(Args, Debug)]
pub struct DecisionArgs {
    #[command(subcommand)]
    pub command: DecisionCommand,
}

#[derive(Subcommand, Debug)]
pub enum DecisionCommand {
    /// Record a new decision
    Add(AddArgs),
    /// Edit an existing decision (only the fields you pass change)
    Edit(EditArgs),
    /// Remove decision(s) by id
    Rm(RmArgs),
    /// List all recorded decisions
    List(ListArgs),
}

#[derive(Args, Debug)]
pub struct AddArgs {
    /// Short title of the decision
    pub title: String,
    /// Why the decision was needed (Context section)
    #[arg(long)]
    pub context: Option<String>,
    /// What was decided (Decision section)
    #[arg(long = "decision")]
    pub decision_text: Option<String>,
    /// Resulting trade-offs (Consequences section)
    #[arg(long)]
    pub consequences: Option<String>,
    /// Code path glob this decision governs (repeatable), e.g. `src/finance/**`
    #[arg(long)]
    pub area: Vec<String>,
    /// Tap to pull provenance snapshots from (must be a configured tap)
    #[arg(long)]
    pub tap: Option<String>,
    /// Document ref(s) to pull from `--tap` (repeatable), e.g. a Notion page id/url
    #[arg(long)]
    pub doc: Vec<String>,
    /// Decision id(s) this one supersedes (marks them superseded)
    #[arg(long)]
    pub supersedes: Vec<u32>,
    /// Decision id(s) this one overrides in overlapping areas
    #[arg(long)]
    pub overrides: Vec<u32>,
    /// Related decision id(s)
    #[arg(long = "relates-to")]
    pub relates_to: Vec<u32>,
    /// Author (defaults to `git config user.name`)
    #[arg(long)]
    pub author: Option<String>,
    /// Status: proposed | accepted | superseded | deprecated (default accepted)
    #[arg(long)]
    pub status: Option<String>,
}

#[derive(Args, Debug)]
pub struct EditArgs {
    /// Id of the decision to edit
    pub id: u32,
    #[arg(long)]
    pub title: Option<String>,
    #[arg(long)]
    pub context: Option<String>,
    #[arg(long = "decision")]
    pub decision_text: Option<String>,
    #[arg(long)]
    pub consequences: Option<String>,
    /// Replace the governed areas (repeatable)
    #[arg(long)]
    pub area: Vec<String>,
    /// Pull additional provenance from this configured tap
    #[arg(long)]
    pub tap: Option<String>,
    /// Document ref(s) to pull from `--tap` (repeatable)
    #[arg(long)]
    pub doc: Vec<String>,
    /// Replace the superseded list (repeatable)
    #[arg(long)]
    pub supersedes: Vec<u32>,
    /// Replace the overrides list (repeatable)
    #[arg(long)]
    pub overrides: Vec<u32>,
    /// Replace the related list (repeatable)
    #[arg(long = "relates-to")]
    pub relates_to: Vec<u32>,
    #[arg(long)]
    pub author: Option<String>,
    #[arg(long)]
    pub status: Option<String>,
}

#[derive(Args, Debug)]
pub struct RmArgs {
    /// Decision id(s) to remove
    #[arg(required = true)]
    pub ids: Vec<u32>,
}

#[derive(Args, Debug)]
pub struct ListArgs {
    /// Human-readable output instead of JSON
    #[arg(long)]
    pub pretty: bool,
}

// ── Dispatch ─────────────────────────────────────────────────────────────────

pub async fn run(args: DecisionArgs) -> Result<()> {
    match args.command {
        DecisionCommand::Add(a) => run_add(a).await,
        DecisionCommand::Edit(a) => run_edit(a).await,
        DecisionCommand::Rm(a) => run_rm(a).await,
        DecisionCommand::List(a) => run_list(a).await,
    }
}

/// Shared store/embedder handle plus the resolved decision namespace.
struct DecisionCtx {
    config: Config,
    store: Box<dyn VectorStore>,
    embedder: Box<dyn Embedder>,
    ns: Namespace,
    dimension: usize,
}

async fn setup() -> Result<DecisionCtx> {
    let config = Config::new()?;
    config.store.validate()?;
    config.embed.validate()?;
    let base = resolve_namespace(&config)?;
    let ns = decision_namespace(&base);
    let embedder = build_embedder(&config.embed).await?;
    let dimension = embedder.dimension();
    let store = build_store(&config.store, dimension)?;
    Ok(DecisionCtx {
        config,
        store,
        embedder,
        ns,
        dimension,
    })
}

/// The decision namespace for a base namespace (`<base>--decision`).
pub fn decision_namespace(base: &Namespace) -> Namespace {
    match namespace_suffix(TAP_NAME) {
        None => base.clone(),
        Some(suffix) => Namespace::from(format!("{}{suffix}", base.as_str())),
    }
}

// ── add ──────────────────────────────────────────────────────────────────────

async fn run_add(args: AddArgs) -> Result<()> {
    let ctx = setup().await?;

    // Ensure the namespace exists and carries our embedder identity.
    if !ctx.store.namespace_exists(&ctx.ns).await? {
        ctx.store.create_namespace(&ctx.ns, ctx.dimension).await?;
    }
    let mut meta = ctx.store.get_metadata(&ctx.ns).await?;
    if meta.embedder.is_none() {
        meta.embedder = Some(embedder_identity(ctx.embedder.as_ref()));
    }
    let mut reg = load_registry(&meta)?;

    let id = reg.next_id();
    let now = now_unix_secs();
    let status = parse_status(args.status.as_deref())?;
    let author = resolve_author(args.author);
    let sources = pull_sources(&ctx.config, args.tap.as_deref(), &args.doc).await?;

    warn_missing_refs(&reg, &args.supersedes, "supersedes");
    warn_missing_refs(&reg, &args.overrides, "overrides");
    warn_missing_refs(&reg, &args.relates_to, "relates-to");

    let entry = DecisionEntry {
        id,
        title: args.title,
        status,
        author,
        date: now,
        updated_at: None,
        context: args.context,
        decision: args.decision_text,
        consequences: args.consequences,
        sources,
        areas: args.area,
        supersedes: args.supersedes.clone(),
        overrides: args.overrides,
        relates_to: args.relates_to,
        superseded_by: None,
    };

    write_decision(&ctx, &entry, now).await?;

    reg.upsert(entry);
    for old in &args.supersedes {
        reg.mark_superseded(*old, id);
    }
    persist_registry(&ctx, &mut meta, &reg).await?;

    eprintln!(
        "  {} {} — {}",
        "recorded".if_supports_color(Stream::Stderr, |s| s.green()),
        decision_uri_for(id).if_supports_color(Stream::Stderr, |s| s.cyan()),
        reg.get(id).map(|e| e.title.as_str()).unwrap_or_default(),
    );
    Ok(())
}

// ── edit ─────────────────────────────────────────────────────────────────────

async fn run_edit(args: EditArgs) -> Result<()> {
    let ctx = setup().await?;
    if !ctx.store.namespace_exists(&ctx.ns).await? {
        bail!("no decisions recorded yet; use `wdpkr decision add` first");
    }
    let mut meta = ctx.store.get_metadata(&ctx.ns).await?;
    let mut reg = load_registry(&meta)?;

    let mut entry = reg
        .get(args.id)
        .cloned()
        .ok_or_else(|| anyhow!("decision {} not found", args.id))?;

    let now = now_unix_secs();
    let mut content_changed = false;

    if let Some(t) = args.title {
        entry.title = t;
        content_changed = true;
    }
    if let Some(c) = args.context {
        entry.context = Some(c);
        content_changed = true;
    }
    if let Some(d) = args.decision_text {
        entry.decision = Some(d);
        content_changed = true;
    }
    if let Some(c) = args.consequences {
        entry.consequences = Some(c);
        content_changed = true;
    }
    if !args.area.is_empty() {
        entry.areas = args.area;
    }
    if !args.supersedes.is_empty() {
        entry.supersedes = args.supersedes.clone();
    }
    if !args.overrides.is_empty() {
        entry.overrides = args.overrides;
    }
    if !args.relates_to.is_empty() {
        entry.relates_to = args.relates_to;
    }
    if let Some(a) = args.author {
        entry.author = a;
    }
    if let Some(s) = args.status {
        entry.status = s.parse::<DecisionStatus>()?;
    }
    let pulled = pull_sources(&ctx.config, args.tap.as_deref(), &args.doc).await?;
    if !pulled.is_empty() {
        entry.sources.extend(pulled);
        content_changed = true;
    }
    entry.updated_at = Some(now);

    if content_changed {
        write_decision(&ctx, &entry, now).await?;
    }

    let supersedes = entry.supersedes.clone();
    reg.upsert(entry);
    for old in &supersedes {
        reg.mark_superseded(*old, args.id);
    }
    persist_registry(&ctx, &mut meta, &reg).await?;

    eprintln!(
        "  {} {}",
        "updated".if_supports_color(Stream::Stderr, |s| s.green()),
        decision_uri_for(args.id).if_supports_color(Stream::Stderr, |s| s.cyan()),
    );
    Ok(())
}

// ── rm ───────────────────────────────────────────────────────────────────────

async fn run_rm(args: RmArgs) -> Result<()> {
    let ctx = setup().await?;
    if !ctx.store.namespace_exists(&ctx.ns).await? {
        bail!("no decisions recorded yet");
    }
    let mut meta = ctx.store.get_metadata(&ctx.ns).await?;
    let mut reg = load_registry(&meta)?;

    let mut removed = 0usize;
    for id in &args.ids {
        match reg.remove(*id) {
            Some(_) => {
                ctx.store
                    .delete_by_file(&ctx.ns, &decision_uri_for(*id))
                    .await?;
                removed += 1;
                eprintln!(
                    "  {} {}",
                    "removed".if_supports_color(Stream::Stderr, |s| s.yellow()),
                    decision_uri_for(*id).if_supports_color(Stream::Stderr, |s| s.cyan()),
                );
            }
            None => eprintln!(
                "  {} decision {id} not found",
                "—".if_supports_color(Stream::Stderr, |s| s.dimmed()),
            ),
        }
    }

    if removed > 0 {
        persist_registry(&ctx, &mut meta, &reg).await?;
    }
    eprintln!("Removed {removed} decision(s)");
    Ok(())
}

// ── list ─────────────────────────────────────────────────────────────────────

async fn run_list(args: ListArgs) -> Result<()> {
    let ctx = setup().await?;
    let reg = if ctx.store.namespace_exists(&ctx.ns).await? {
        load_registry(&ctx.store.get_metadata(&ctx.ns).await?)?
    } else {
        DecisionRegistry::default()
    };

    if args.pretty {
        if reg.decisions.is_empty() {
            println!("No decisions recorded.");
            return Ok(());
        }
        for d in &reg.decisions {
            let areas = if d.areas.is_empty() {
                String::new()
            } else {
                format!("  [{}]", d.areas.join(", "))
            };
            println!(
                "{}  {:<10} {}{}  ({})",
                d.uri().if_supports_color(Stream::Stdout, |s| s.cyan()),
                d.status.as_str(),
                d.title,
                areas.if_supports_color(Stream::Stdout, |s| s.dimmed()),
                d.author.if_supports_color(Stream::Stdout, |s| s.dimmed()),
            );
        }
    } else {
        println!("{}", serde_json::to_string_pretty(&reg.decisions)?);
    }
    Ok(())
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn decision_uri_for(id: u32) -> String {
    crate::decision::decision_uri(id)
}

fn load_registry(meta: &NamespaceMetadata) -> Result<DecisionRegistry> {
    match meta.extra.get(REGISTRY_META_KEY) {
        Some(json) => DecisionRegistry::from_json(json),
        None => Ok(DecisionRegistry::default()),
    }
}

async fn persist_registry(
    ctx: &DecisionCtx,
    meta: &mut NamespaceMetadata,
    reg: &DecisionRegistry,
) -> Result<()> {
    meta.extra
        .insert(REGISTRY_META_KEY.to_string(), reg.to_json()?);
    ctx.store.set_metadata(&ctx.ns, meta).await
}

/// Embed a decision and (re)upsert its documents, clearing any prior vectors for
/// the same id first (ids are content-addressed, so edits would otherwise leave
/// stale vectors behind).
async fn write_decision(ctx: &DecisionCtx, entry: &DecisionEntry, now: i64) -> Result<()> {
    let item = entry.to_source_item();
    let result = process_item(&item, None, ctx.embedder.as_ref(), EmbedMode::Docstring).await?;
    let mut docs = result.documents;
    for d in &mut docs {
        d.last_used_at = Some(now);
    }
    ctx.store.delete_by_file(&ctx.ns, &entry.uri()).await?;
    ctx.store.upsert(&ctx.ns, &docs).await?;
    Ok(())
}

fn parse_status(s: Option<&str>) -> Result<DecisionStatus> {
    match s {
        Some(v) => v.parse::<DecisionStatus>(),
        None => Ok(DecisionStatus::default()),
    }
}

/// Resolve the author: explicit `--author`, else `git config user.name`, else
/// "unknown".
fn resolve_author(explicit: Option<String>) -> String {
    explicit
        .filter(|s| !s.trim().is_empty())
        .or_else(git_user_name)
        .unwrap_or_else(|| "unknown".to_string())
}

fn git_user_name() -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["config", "user.name"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!name.is_empty()).then_some(name)
}

fn warn_missing_refs(reg: &DecisionRegistry, ids: &[u32], label: &str) {
    for id in ids {
        if reg.get(*id).is_none() {
            eprintln!(
                "  {} {label} references decision {id}, which does not exist",
                "warning:".if_supports_color(Stream::Stderr, |s| s.yellow()),
            );
        }
    }
}

/// Pull provenance snapshots by building the named tap with the given `--doc`
/// targets and fetching. Errors clearly if `--doc` is given without `--tap`, or
/// the tap isn't configured.
async fn pull_sources(
    config: &Config,
    tap_name: Option<&str>,
    docs: &[String],
) -> Result<Vec<SourceRef>> {
    match tap_name {
        None => {
            if !docs.is_empty() {
                bail!("--doc requires --tap <name>");
            }
            Ok(vec![])
        }
        Some(name) => {
            if docs.is_empty() {
                bail!("--tap requires at least one --doc <ref>");
            }
            let cfg = config.taps.iter().find(|t| t.name == name).ok_or_else(|| {
                let configured: Vec<&str> = config.taps.iter().map(|t| t.name.as_str()).collect();
                anyhow!(
                    "tap '{name}' is not configured; configured taps: {}",
                    configured.join(", ")
                )
            })?;
            let root = std::env::current_dir()?;
            let tap = build_tap(cfg, root, docs)?;
            pull_from_tap(tap.as_ref()).await
        }
    }
}

/// Fetch a tap's items and turn them into capped provenance snapshots.
async fn pull_from_tap(tap: &dyn Tap) -> Result<Vec<SourceRef>> {
    let ctx = FetchContext {
        full: true,
        cursor: None,
        stored_hashes: HashMap::new(),
    };
    let result = tap.fetch(&ctx).await?;
    Ok(result
        .items
        .into_iter()
        .map(|it| SourceRef {
            uri: it.source_path,
            snapshot: it.content,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::mock_tap::MockTap;

    #[test]
    fn decision_namespace_appends_suffix() {
        let base = Namespace::from("repo");
        assert_eq!(decision_namespace(&base).as_str(), "repo--decision");
    }

    #[test]
    fn parse_status_defaults_to_accepted() {
        assert_eq!(parse_status(None).unwrap(), DecisionStatus::Accepted);
        assert_eq!(
            parse_status(Some("superseded")).unwrap(),
            DecisionStatus::Superseded
        );
        assert!(parse_status(Some("nope")).is_err());
    }

    #[test]
    fn resolve_author_prefers_explicit() {
        assert_eq!(resolve_author(Some("Grace".into())), "Grace");
        // blank explicit falls through to git/unknown (non-empty regardless)
        assert!(!resolve_author(Some("   ".into())).is_empty());
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn pull_from_tap_snapshots_items() {
        use crate::tap::SourceItem;
        let items = vec![SourceItem {
            source_path: "notion://abc".into(),
            content: "Half-up rounding at 2 decimals".into(),
            content_hash: "h".into(),
            language: None,
            module_doc: None,
            children: vec![],
        }];
        let tap = MockTap::new("notion", items);
        let sources = pull_from_tap(&tap).await.unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].uri, "notion://abc");
        assert!(sources[0].snapshot.contains("Half-up rounding"));
    }

    #[test]
    fn parse_add_args() {
        use crate::cli::{Cli, Command};
        use clap::Parser;
        let cli = Cli::try_parse_from([
            "wdpkr",
            "decision",
            "add",
            "Rounding policy",
            "--context",
            "why",
            "--decision",
            "round half up",
            "--area",
            "src/finance/**",
            "--tap",
            "notion",
            "--doc",
            "399cb",
            "--supersedes",
            "2",
            "--overrides",
            "3",
        ])
        .unwrap();
        match cli.command {
            Command::Decision(DecisionArgs {
                command: DecisionCommand::Add(a),
            }) => {
                assert_eq!(a.title, "Rounding policy");
                assert_eq!(a.context.as_deref(), Some("why"));
                assert_eq!(a.decision_text.as_deref(), Some("round half up"));
                assert_eq!(a.area, vec!["src/finance/**".to_string()]);
                assert_eq!(a.tap.as_deref(), Some("notion"));
                assert_eq!(a.doc, vec!["399cb".to_string()]);
                assert_eq!(a.supersedes, vec![2]);
                assert_eq!(a.overrides, vec![3]);
            }
            _ => panic!("expected decision add"),
        }
    }

    #[test]
    fn parse_edit_and_rm_and_list() {
        use crate::cli::{Cli, Command};
        use clap::Parser;

        let cli = Cli::try_parse_from(["wdpkr", "decision", "edit", "5", "--status", "deprecated"])
            .unwrap();
        match cli.command {
            Command::Decision(DecisionArgs {
                command: DecisionCommand::Edit(a),
            }) => {
                assert_eq!(a.id, 5);
                assert_eq!(a.status.as_deref(), Some("deprecated"));
            }
            _ => panic!("expected decision edit"),
        }

        let cli = Cli::try_parse_from(["wdpkr", "decision", "rm", "1", "2", "3"]).unwrap();
        match cli.command {
            Command::Decision(DecisionArgs {
                command: DecisionCommand::Rm(a),
            }) => assert_eq!(a.ids, vec![1, 2, 3]),
            _ => panic!("expected decision rm"),
        }

        assert!(Cli::try_parse_from(["wdpkr", "decision", "rm"]).is_err());

        let cli = Cli::try_parse_from(["wdpkr", "decision", "list", "--pretty"]).unwrap();
        match cli.command {
            Command::Decision(DecisionArgs {
                command: DecisionCommand::List(a),
            }) => assert!(a.pretty),
            _ => panic!("expected decision list"),
        }
    }
}
