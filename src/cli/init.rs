use std::path::Path;

use anyhow::{Context, Result};
use clap::Args;

use super::prompt::{prompt_choice, prompt_confirm, prompt_freetext};

const WDPKR_SECTION: &str = include_str!("templates/claude_md.md");
const WDPKRIGNORE: &str = include_str!("templates/wdpkrignore");
const CI_WORKFLOW: &str = include_str!("templates/ci_workflow.yml");

const SECTION_MARKER: &str = "### wdpkr";

#[derive(Args, Debug)]
pub struct InitArgs {}

pub async fn run(_args: InitArgs) -> Result<()> {
    println!("wdpkr init — setting up wdpkr for this repository\n");

    let mut wrote = Vec::new();
    let mut skipped = Vec::new();

    // ── 1. Agent context file ─────────────────────────────────────────
    let existing: Vec<&str> = ["CLAUDE.md", "AGENTS.md"]
        .into_iter()
        .filter(|p| Path::new(p).exists())
        .collect();

    if existing.is_empty() {
        let choice = prompt_agent_file()?;
        std::fs::write(choice.as_str(), WDPKR_SECTION)
            .with_context(|| format!("writing {choice}"))?;
        wrote.push(format!("{choice} (created)"));
    } else {
        for path in existing {
            try_collect(append_section(path), path, &mut wrote, &mut skipped);
        }
    }

    // ── 2. .wdpkrignore ───────────────────────────────────────────────
    println!();
    try_collect(
        write_if_missing(".wdpkrignore", WDPKRIGNORE),
        ".wdpkrignore",
        &mut wrote,
        &mut skipped,
    );

    // ── 3. Indexer workflow ───────────────────────────────────────────
    println!();
    if prompt_confirm("Add GitHub Actions indexer workflow?", true)? {
        let workflow_dir = ".github/workflows";
        let workflow_path = format!("{workflow_dir}/wdpkr.yml");
        if let Err(e) = std::fs::create_dir_all(workflow_dir) {
            eprintln!("warning: could not create {workflow_dir}: {e}");
        } else {
            try_collect(
                write_if_missing(&workflow_path, CI_WORKFLOW),
                "CI workflow",
                &mut wrote,
                &mut skipped,
            );
        }

        println!();
        println!("The workflow requires these GitHub Actions secrets:");
        println!("  TURBOPUFFER_API_KEY   — vector store");
        println!("  VOYAGE_API_KEY        — embeddings");
        println!("  ANTHROPIC_API_KEY     — summarization");
        println!();
        println!("Add them in your repo: Settings → Secrets and variables → Actions");
    }

    // ── Summary ───────────────────────────────────────────────────────
    println!();
    if !wrote.is_empty() {
        println!("Wrote:");
        for p in &wrote {
            println!("  {p}");
        }
    }
    if !skipped.is_empty() {
        println!("Skipped (already present):");
        for p in &skipped {
            println!("  {p}");
        }
    }
    if wrote.is_empty() && skipped.is_empty() {
        println!("Nothing to do — wdpkr is already initialized.");
    }

    println!();
    println!(
        "Next: run `wdpkr config init` to configure API keys and providers for indexing and search."
    );

    Ok(())
}

fn prompt_agent_file() -> Result<String> {
    let choice = prompt_choice(
        "Agent context file",
        &["CLAUDE.md", "AGENTS.md", "Other"],
        "CLAUDE.md",
    )?;
    if choice == "Other" {
        return prompt_freetext("Filename", "CONTEXT.md");
    }
    Ok(choice)
}

enum WriteAction {
    Wrote(String),
    Skipped(String),
}

fn try_collect(
    result: Result<WriteAction>,
    label: &str,
    wrote: &mut Vec<String>,
    skipped: &mut Vec<String>,
) {
    match result {
        Ok(WriteAction::Wrote(p)) => wrote.push(p),
        Ok(WriteAction::Skipped(p)) => skipped.push(p),
        Err(e) => eprintln!("warning: {label}: {e}"),
    }
}

fn write_if_missing(path: &str, content: &str) -> Result<WriteAction> {
    if Path::new(path).exists() {
        return Ok(WriteAction::Skipped(path.to_string()));
    }
    std::fs::write(path, content).with_context(|| format!("writing {path}"))?;
    Ok(WriteAction::Wrote(path.to_string()))
}

fn append_section(path: &str) -> Result<WriteAction> {
    let existing = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
    if existing.contains(SECTION_MARKER) {
        return Ok(WriteAction::Skipped(format!("{path} (section exists)")));
    }
    let mut content = existing;
    if !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str(WDPKR_SECTION);
    std::fs::write(path, content).with_context(|| format!("writing {path}"))?;
    Ok(WriteAction::Wrote(format!("{path} (appended)")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tempdir(label: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "wdpkr-init-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn section_template_contains_marker() {
        assert!(WDPKR_SECTION.contains(SECTION_MARKER));
        assert!(WDPKR_SECTION.contains("wdpkr search"));
    }

    #[test]
    fn wdpkrignore_template_has_lockfiles() {
        assert!(WDPKRIGNORE.contains("package-lock.json"));
        assert!(WDPKRIGNORE.contains("go.sum"));
        assert!(WDPKRIGNORE.contains("Cargo.lock"));
    }

    #[test]
    fn ci_workflow_template_has_cargo_install() {
        assert!(CI_WORKFLOW.contains("cargo install wdpkr"));
        assert!(CI_WORKFLOW.contains("TURBOPUFFER_API_KEY"));
    }

    #[test]
    fn write_if_missing_creates_file() {
        let dir = tempdir("create");
        let path = dir.join("test.txt");
        let path_str = path.to_str().unwrap();
        let action = write_if_missing(path_str, "hello").unwrap();
        assert!(matches!(action, WriteAction::Wrote(_)));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn write_if_missing_skips_existing() {
        let dir = tempdir("skip");
        let path = dir.join("test.txt");
        std::fs::write(&path, "existing").unwrap();
        let path_str = path.to_str().unwrap();
        let action = write_if_missing(path_str, "new content").unwrap();
        assert!(matches!(action, WriteAction::Skipped(_)));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "existing");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn append_section_adds_to_existing_file() {
        let dir = tempdir("append");
        let path = dir.join("CLAUDE.md");
        std::fs::write(&path, "# My Project\n\nExisting content.\n").unwrap();
        let path_str = path.to_str().unwrap();
        let action = append_section(path_str).unwrap();
        assert!(matches!(action, WriteAction::Wrote(_)));
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("# My Project"));
        assert!(content.contains(SECTION_MARKER));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn append_section_skips_if_already_present() {
        let dir = tempdir("present");
        let path = dir.join("CLAUDE.md");
        std::fs::write(&path, format!("# My Project\n\n{WDPKR_SECTION}")).unwrap();
        let path_str = path.to_str().unwrap();
        let action = append_section(path_str).unwrap();
        assert!(matches!(action, WriteAction::Skipped(_)));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn append_section_works_on_agents_md() {
        let dir = tempdir("agents");
        let path = dir.join("AGENTS.md");
        std::fs::write(&path, "# Agent Instructions\n").unwrap();
        let path_str = path.to_str().unwrap();
        let action = append_section(path_str).unwrap();
        assert!(matches!(action, WriteAction::Wrote(_)));
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("# Agent Instructions"));
        assert!(content.contains(SECTION_MARKER));
        std::fs::remove_dir_all(&dir).ok();
    }
}
