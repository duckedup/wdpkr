use super::{FileConfig, env_or, file_or};
use anyhow::{Result, bail};

pub struct EmbedConfig {
    pub provider: String,
    pub model: String,
    pub batch_size: usize,

    pub voyage_api_key: String,
    pub openai_api_key: String,
    pub ollama_host: String,
}

impl EmbedConfig {
    pub fn from_env(file: &Option<FileConfig>) -> Self {
        let f = file.as_ref().and_then(|f| f.embedder.as_ref());

        let provider = env_or(
            "MEGAGREP_EMBED_PROVIDER",
            file_or(f.and_then(|e| e.provider.clone()), "voyage".into()),
        );

        // Default model is provider-derived: setting MEGAGREP_EMBED_PROVIDER
        // alone must yield a sensible model for that provider — see SPEC §
        // EmbedConfig.
        let default_model = match provider.as_str() {
            "voyage" => "voyage-code-3",
            "ollama" => "nomic-embed-text",
            "openai" => "text-embedding-3-large",
            _ => "voyage-code-3",
        };

        Self {
            provider: provider.clone(),
            model: env_or(
                "MEGAGREP_EMBED_MODEL",
                file_or(f.and_then(|e| e.model.clone()), default_model.into()),
            ),
            batch_size: env_or(
                "MEGAGREP_EMBED_BATCH_SIZE",
                file_or(f.and_then(|e| e.batch_size), 64),
            ),
            voyage_api_key: env_or("VOYAGE_API_KEY", String::new()),
            openai_api_key: env_or("OPENAI_API_KEY", String::new()),
            ollama_host: env_or(
                "OLLAMA_HOST",
                file_or(
                    f.and_then(|e| e.ollama_host.clone()),
                    "http://localhost:11434".into(),
                ),
            ),
        }
    }

    /// Validate that the selected provider's required credential is set.
    /// Called by the indexer/searcher at startup — fail fast, not on first
    /// API call. Not called by `Config::new` itself, so subcommands that do
    /// not hit the embedder (e.g. `megagrep config get`) work without keys.
    pub fn validate(&self) -> Result<()> {
        match self.provider.as_str() {
            "voyage" if self.voyage_api_key.is_empty() => {
                bail!("VOYAGE_API_KEY is required when embedder.provider=voyage")
            }
            "openai" if self.openai_api_key.is_empty() => {
                bail!("OPENAI_API_KEY is required when embedder.provider=openai")
            }
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::FileEmbedConfig;
    use crate::config::test_helpers::{remove_envs, set_env};
    use serial_test::serial;

    fn clear_env() {
        remove_envs(&[
            "MEGAGREP_EMBED_PROVIDER",
            "MEGAGREP_EMBED_MODEL",
            "MEGAGREP_EMBED_BATCH_SIZE",
            "VOYAGE_API_KEY",
            "OPENAI_API_KEY",
            "OLLAMA_HOST",
        ]);
    }

    #[test]
    #[serial]
    fn defaults() {
        clear_env();
        let cfg = EmbedConfig::from_env(&None);
        assert_eq!(cfg.provider, "voyage");
        assert_eq!(cfg.model, "voyage-code-3");
        assert_eq!(cfg.batch_size, 64);
        assert_eq!(cfg.ollama_host, "http://localhost:11434");
        assert_eq!(cfg.voyage_api_key, "");
    }

    #[test]
    #[serial]
    fn ollama_provider_picks_nomic_default_model() {
        clear_env();
        set_env("MEGAGREP_EMBED_PROVIDER", "ollama");
        let cfg = EmbedConfig::from_env(&None);
        assert_eq!(cfg.provider, "ollama");
        assert_eq!(cfg.model, "nomic-embed-text");
        clear_env();
    }

    #[test]
    #[serial]
    fn openai_provider_picks_3_large_default_model() {
        clear_env();
        set_env("MEGAGREP_EMBED_PROVIDER", "openai");
        let cfg = EmbedConfig::from_env(&None);
        assert_eq!(cfg.provider, "openai");
        assert_eq!(cfg.model, "text-embedding-3-large");
        clear_env();
    }

    #[test]
    #[serial]
    fn explicit_model_overrides_provider_default() {
        clear_env();
        set_env("MEGAGREP_EMBED_PROVIDER", "ollama");
        set_env("MEGAGREP_EMBED_MODEL", "mxbai-embed-large");
        let cfg = EmbedConfig::from_env(&None);
        assert_eq!(cfg.model, "mxbai-embed-large");
        clear_env();
    }

    #[test]
    #[serial]
    fn batch_size_env_override() {
        clear_env();
        set_env("MEGAGREP_EMBED_BATCH_SIZE", "128");
        let cfg = EmbedConfig::from_env(&None);
        assert_eq!(cfg.batch_size, 128);
        clear_env();
    }

    #[test]
    #[serial]
    fn unknown_provider_falls_through_to_voyage_default_model() {
        clear_env();
        set_env("MEGAGREP_EMBED_PROVIDER", "made-up");
        let cfg = EmbedConfig::from_env(&None);
        assert_eq!(cfg.provider, "made-up");
        assert_eq!(cfg.model, "voyage-code-3");
        clear_env();
    }

    #[test]
    #[serial]
    fn file_value_used_when_env_absent() {
        clear_env();
        let file = FileConfig {
            embedder: Some(FileEmbedConfig {
                provider: Some("openai".into()),
                model: None,
                batch_size: Some(16),
                ollama_host: Some("http://ollama.internal:11434".into()),
            }),
            ..Default::default()
        };
        let cfg = EmbedConfig::from_env(&Some(file));
        assert_eq!(cfg.provider, "openai");
        // Model unspecified in file → provider-derived default kicks in
        assert_eq!(cfg.model, "text-embedding-3-large");
        assert_eq!(cfg.batch_size, 16);
        assert_eq!(cfg.ollama_host, "http://ollama.internal:11434");
    }

    #[test]
    #[serial]
    fn env_beats_file_for_provider() {
        clear_env();
        set_env("MEGAGREP_EMBED_PROVIDER", "voyage");
        let file = FileConfig {
            embedder: Some(FileEmbedConfig {
                provider: Some("ollama".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let cfg = EmbedConfig::from_env(&Some(file));
        assert_eq!(cfg.provider, "voyage");
        // Model default tracks the env-resolved provider, not the file's
        assert_eq!(cfg.model, "voyage-code-3");
        clear_env();
    }

    #[test]
    #[serial]
    fn validate_fails_for_voyage_without_key() {
        clear_env();
        let cfg = EmbedConfig::from_env(&None);
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("VOYAGE_API_KEY"));
    }

    #[test]
    #[serial]
    fn validate_passes_for_voyage_with_key() {
        clear_env();
        set_env("VOYAGE_API_KEY", "key-abc");
        let cfg = EmbedConfig::from_env(&None);
        assert!(cfg.validate().is_ok());
        clear_env();
    }

    #[test]
    #[serial]
    fn validate_passes_for_ollama_without_any_key() {
        clear_env();
        set_env("MEGAGREP_EMBED_PROVIDER", "ollama");
        let cfg = EmbedConfig::from_env(&None);
        assert!(cfg.validate().is_ok());
        clear_env();
    }

    #[test]
    #[serial]
    fn validate_fails_for_openai_without_key() {
        clear_env();
        set_env("MEGAGREP_EMBED_PROVIDER", "openai");
        let cfg = EmbedConfig::from_env(&None);
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("OPENAI_API_KEY"));
        clear_env();
    }
}
