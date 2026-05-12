use std::path::Path;

use anyhow::{Context, Result};
use clap::Args;

const MEGAGREP_SECTION: &str = include_str!("templates/claude_md.md");
const MEGAGREPIGNORE: &str = include_str!("templates/megagrepignore");
const CI_WORKFLOW: &str = include_str!("templates/ci_workflow.yml");

const SECTION_MARKER: &str = "### megagrep";

#[derive(Args, Debug)]
pub struct InitArgs {}

pub async fn run(_args: InitArgs) -> Result<()> {
    let mut wrote = Vec::new();
    let mut skipped = Vec::new();

    // 1. Agent instruction files (CLAUDE.md / AGENTS.md)
    let claude_exists = Path::new("CLAUDE.md").exists();
    let agents_exists = Path::new("AGENTS.md").exists();

    if claude_exists {
        match append_section("CLAUDE.md") {
            Ok(action) => match action {
                WriteAction::Appended(p) => wrote.push(p),
                WriteAction::Skipped(p) => skipped.push(p),
                _ => {}
            },
            Err(e) => eprintln!("warning: CLAUDE.md: {e}"),
        }
    }

    if agents_exists {
        match append_section("AGENTS.md") {
            Ok(action) => match action {
                WriteAction::Appended(p) => wrote.push(p),
                WriteAction::Skipped(p) => skipped.push(p),
                _ => {}
            },
            Err(e) => eprintln!("warning: AGENTS.md: {e}"),
        }
    }

    if !claude_exists && !agents_exists {
        let choice = prompt_agent_file_choice()?;
        std::fs::write(choice.as_str(), MEGAGREP_SECTION)
            .with_context(|| format!("writing {choice}"))?;
        wrote.push(format!("{choice} (created)"));
    }

    // 2. .megagrepignore
    match write_if_missing(".megagrepignore", MEGAGREPIGNORE) {
        Ok(action) => match action {
            WriteAction::Created(p) => wrote.push(p),
            WriteAction::Skipped(p) => skipped.push(p),
            _ => {}
        },
        Err(e) => eprintln!("warning: .megagrepignore: {e}"),
    }

    // 3. CI workflow
    let workflow_dir = ".github/workflows";
    let workflow_path = format!("{workflow_dir}/megagrep.yml");
    if let Err(e) = std::fs::create_dir_all(workflow_dir) {
        eprintln!("warning: could not create {workflow_dir}: {e}");
    } else {
        match write_if_missing(&workflow_path, CI_WORKFLOW) {
            Ok(action) => match action {
                WriteAction::Created(p) => wrote.push(p),
                WriteAction::Skipped(p) => skipped.push(p),
                _ => {}
            },
            Err(e) => eprintln!("warning: CI workflow: {e}"),
        }
    }

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
        println!("Nothing to do — megagrep is already initialized.");
    }

    Ok(())
}

enum WriteAction {
    Created(String),
    Appended(String),
    Skipped(String),
}

fn write_if_missing(path: &str, content: &str) -> Result<WriteAction> {
    if Path::new(path).exists() {
        return Ok(WriteAction::Skipped(path.to_string()));
    }
    std::fs::write(path, content).with_context(|| format!("writing {path}"))?;
    Ok(WriteAction::Created(path.to_string()))
}

fn prompt_agent_file_choice() -> Result<String> {
    eprintln!("No CLAUDE.md or AGENTS.md found. Where should the megagrep agent instructions go?");
    eprintln!("  1) CLAUDE.md");
    eprintln!("  2) AGENTS.md");
    eprint!("Choice [1]: ");

    let mut input = String::new();
    std::io::stdin()
        .read_line(&mut input)
        .context("reading user input")?;
    parse_agent_file_choice(input.trim())
}

fn parse_agent_file_choice(input: &str) -> Result<String> {
    match input {
        "" | "1" => Ok("CLAUDE.md".to_string()),
        "2" => Ok("AGENTS.md".to_string()),
        other => anyhow::bail!("invalid choice: '{other}' — expected 1 or 2"),
    }
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
    content.push_str(MEGAGREP_SECTION);
    std::fs::write(path, content).with_context(|| format!("writing {path}"))?;
    Ok(WriteAction::Appended(format!("{path} (appended)")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tempdir(label: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "megagrep-init-{label}-{}-{}",
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
        assert!(MEGAGREP_SECTION.contains(SECTION_MARKER));
        assert!(MEGAGREP_SECTION.contains("megagrep search"));
    }

    #[test]
    fn megagrepignore_template_has_lockfiles() {
        assert!(MEGAGREPIGNORE.contains("package-lock.json"));
        assert!(MEGAGREPIGNORE.contains("go.sum"));
        assert!(MEGAGREPIGNORE.contains("Cargo.lock"));
    }

    #[test]
    fn ci_workflow_template_has_cargo_install() {
        assert!(CI_WORKFLOW.contains("cargo install megagrep"));
        assert!(CI_WORKFLOW.contains("TURBOPUFFER_API_KEY"));
    }

    #[test]
    fn write_if_missing_creates_file() {
        let dir = tempdir("create");
        let path = dir.join("test.txt");
        let path_str = path.to_str().unwrap();
        let action = write_if_missing(path_str, "hello").unwrap();
        assert!(matches!(action, WriteAction::Created(_)));
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
        assert!(matches!(action, WriteAction::Appended(_)));
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("# My Project"));
        assert!(content.contains(SECTION_MARKER));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn append_section_skips_if_already_present() {
        let dir = tempdir("present");
        let path = dir.join("CLAUDE.md");
        std::fs::write(&path, format!("# My Project\n\n{MEGAGREP_SECTION}")).unwrap();
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
        assert!(matches!(action, WriteAction::Appended(_)));
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("# Agent Instructions"));
        assert!(content.contains(SECTION_MARKER));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn choice_default_is_claude_md() {
        assert_eq!(parse_agent_file_choice("").unwrap(), "CLAUDE.md");
    }

    #[test]
    fn choice_1_is_claude_md() {
        assert_eq!(parse_agent_file_choice("1").unwrap(), "CLAUDE.md");
    }

    #[test]
    fn choice_2_is_agents_md() {
        assert_eq!(parse_agent_file_choice("2").unwrap(), "AGENTS.md");
    }

    #[test]
    fn choice_invalid_errors() {
        assert!(parse_agent_file_choice("3").is_err());
        assert!(parse_agent_file_choice("claude").is_err());
    }
}
