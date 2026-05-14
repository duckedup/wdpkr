//! Git utilities for the indexer: SHA, diff, remote URL, namespace derivation.
//!
//! All git commands accept an optional directory parameter so the indexer
//! can run against an arbitrary repo root (not just cwd). This is critical
//! for integration testing with temp git repos.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

/// Run a git command in the given directory and return stdout.
fn git_in(dir: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .with_context(|| format!("running git {}", args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn current_sha(dir: &Path) -> Result<String> {
    git_in(dir, &["rev-parse", "HEAD"])
}

pub fn remote_url(dir: &Path, remote_name: &str) -> Result<String> {
    git_in(dir, &["remote", "get-url", remote_name])
}

pub struct DiffResult {
    pub changed: Vec<String>,
    pub deleted: Vec<String>,
}

pub fn diff_files(dir: &Path, from: &str, to: &str) -> Result<DiffResult> {
    let output = git_in(dir, &["diff", "--name-status", &format!("{from}..{to}")])?;

    let mut changed = Vec::new();
    let mut deleted = Vec::new();

    for line in output.lines() {
        let parts: Vec<&str> = line.splitn(2, '\t').collect();
        if parts.len() != 2 {
            continue;
        }
        let (status, path) = (parts[0], parts[1]);
        match status {
            "D" => deleted.push(path.to_string()),
            _ => changed.push(path.to_string()),
        }
    }

    Ok(DiffResult { changed, deleted })
}

pub fn derive_namespace(remote_url: &str) -> String {
    let normalized = remote_url
        .trim()
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .replace("ssh://", "")
        .replace("https://", "")
        .replace("http://", "")
        .replace("git@", "")
        .replace(':', "/");
    let hash = blake3::hash(normalized.as_bytes());
    let short_hash = &hash.to_hex()[..12];
    let name_part = normalized
        .rsplit('/')
        .next()
        .unwrap_or("repo")
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
        .collect::<String>();
    format!("{name_part}-{short_hash}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_sha_returns_hex() {
        let sha = current_sha(Path::new(".")).unwrap();
        assert!(!sha.is_empty());
        assert!(sha.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn derive_namespace_from_ssh_url() {
        let ns = derive_namespace("ssh://git@codeberg.org/phoenixai/megagrep.git");
        assert!(ns.starts_with("megagrep-"));
    }

    #[test]
    fn derive_namespace_from_https_url() {
        let ns = derive_namespace("https://github.com/user/repo.git");
        assert!(ns.starts_with("repo-"));
    }

    #[test]
    fn derive_namespace_from_git_at_url() {
        let ns = derive_namespace("git@github.com:user/repo.git");
        assert!(ns.starts_with("repo-"));
    }

    #[test]
    fn derive_namespace_is_deterministic() {
        let a = derive_namespace("https://github.com/user/repo.git");
        let b = derive_namespace("https://github.com/user/repo.git");
        assert_eq!(a, b);
    }

    #[test]
    fn derive_namespace_different_urls_differ() {
        let a = derive_namespace("https://github.com/user/repo-a.git");
        let b = derive_namespace("https://github.com/user/repo-b.git");
        assert_ne!(a, b);
    }
}
