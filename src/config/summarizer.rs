use super::{FileConfig, Resolved, Source, env_or_resolved, file_or_resolved};
use anyhow::{Result, bail};

pub struct SummarizerConfig {
    pub provider: String,
    pub model: String,
    pub api_key: String,
}

#[derive(Debug, Clone)]
pub struct SummarizerSources {
    pub provider: Source,
    pub model: Source,
    pub api_key: Source,
}

impl SummarizerConfig {
    pub fn from_env(file: &Option<FileConfig>) -> Self {
        Self::resolve(file).0
    }

    pub fn resolve(file: &Option<FileConfig>) -> (Self, SummarizerSources) {
        let f = file.as_ref().and_then(|f| f.summarizer.as_ref());

        let provider: Resolved<String> = env_or_resolved(
            "MEGAGREP_SUMMARIZER_PROVIDER",
            file_or_resolved(f.and_then(|s| s.provider.clone()), "anthropic".into()),
        );
        let model: Resolved<String> = env_or_resolved(
            "MEGAGREP_SUMMARIZER_MODEL",
            file_or_resolved(
                f.and_then(|s| s.model.clone()),
                "claude-haiku-4-5-20251001".into(),
            ),
        );
        let api_key: Resolved<String> = env_or_resolved(
            "ANTHROPIC_API_KEY",
            file_or_resolved(f.and_then(|s| s.anthropic_api_key.clone()), String::new()),
        );

        (
            Self {
                provider: provider.value,
                model: model.value,
                api_key: api_key.value,
            },
            SummarizerSources {
                provider: provider.source,
                model: model.source,
                api_key: api_key.source,
            },
        )
    }

    /// Mirror of `EmbedConfig::validate`: the indexer must hit the
    /// summarizer, so missing creds are a fail-fast condition for that path.
    pub fn validate(&self) -> Result<()> {
        match self.provider.as_str() {
            "anthropic" if self.api_key.is_empty() => {
                bail!("ANTHROPIC_API_KEY is required when summarizer.provider=anthropic")
            }
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::FileSummarizerConfig;
    use crate::config::test_helpers::{remove_envs, set_env};
    use serial_test::serial;

    fn clear_env() {
        remove_envs(&[
            "MEGAGREP_SUMMARIZER_PROVIDER",
            "MEGAGREP_SUMMARIZER_MODEL",
            "ANTHROPIC_API_KEY",
        ]);
    }

    #[test]
    #[serial]
    fn defaults() {
        clear_env();
        let cfg = SummarizerConfig::from_env(&None);
        assert_eq!(cfg.provider, "anthropic");
        assert_eq!(cfg.model, "claude-haiku-4-5-20251001");
        assert_eq!(cfg.api_key, "");
    }

    #[test]
    #[serial]
    fn env_overrides() {
        clear_env();
        set_env("MEGAGREP_SUMMARIZER_MODEL", "claude-sonnet-4-6");
        set_env("ANTHROPIC_API_KEY", "key-xyz");
        let cfg = SummarizerConfig::from_env(&None);
        assert_eq!(cfg.model, "claude-sonnet-4-6");
        assert_eq!(cfg.api_key, "key-xyz");
        clear_env();
    }

    #[test]
    #[serial]
    fn file_value_used_when_env_absent() {
        clear_env();
        let file = FileConfig {
            summarizer: Some(FileSummarizerConfig {
                provider: None,
                model: Some("claude-opus-4-7".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let cfg = SummarizerConfig::from_env(&Some(file));
        assert_eq!(cfg.model, "claude-opus-4-7");
    }

    #[test]
    #[serial]
    fn env_beats_file() {
        clear_env();
        set_env("MEGAGREP_SUMMARIZER_MODEL", "claude-sonnet-4-6");
        let file = FileConfig {
            summarizer: Some(FileSummarizerConfig {
                provider: None,
                model: Some("claude-opus-4-7".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let cfg = SummarizerConfig::from_env(&Some(file));
        assert_eq!(cfg.model, "claude-sonnet-4-6");
        clear_env();
    }

    #[test]
    #[serial]
    fn validate_fails_for_anthropic_without_key() {
        clear_env();
        let cfg = SummarizerConfig::from_env(&None);
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("ANTHROPIC_API_KEY"));
    }

    #[test]
    #[serial]
    fn validate_passes_for_anthropic_with_key() {
        clear_env();
        set_env("ANTHROPIC_API_KEY", "key-abc");
        let cfg = SummarizerConfig::from_env(&None);
        assert!(cfg.validate().is_ok());
        clear_env();
    }

    // ── Source attribution ────────────────────────────────────────────

    #[test]
    #[serial]
    fn resolve_marks_default_when_no_input() {
        clear_env();
        let (_, sources) = SummarizerConfig::resolve(&None);
        assert_eq!(sources.provider, Source::Default);
        assert_eq!(sources.model, Source::Default);
        assert_eq!(sources.api_key, Source::Default);
    }

    #[test]
    #[serial]
    fn resolve_marks_file_when_file_only() {
        clear_env();
        let file = FileConfig {
            summarizer: Some(FileSummarizerConfig {
                model: Some("claude-opus-4-7".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let (_, sources) = SummarizerConfig::resolve(&Some(file));
        assert_eq!(sources.model, Source::File);
        assert_eq!(sources.provider, Source::Default);
    }

    #[test]
    #[serial]
    fn resolve_marks_env_when_env_set() {
        clear_env();
        set_env("MEGAGREP_SUMMARIZER_MODEL", "claude-sonnet-4-6");
        set_env("ANTHROPIC_API_KEY", "k");
        let (_, sources) = SummarizerConfig::resolve(&None);
        assert_eq!(sources.model, Source::Env("MEGAGREP_SUMMARIZER_MODEL"));
        assert_eq!(sources.api_key, Source::Env("ANTHROPIC_API_KEY"));
        clear_env();
    }
}
