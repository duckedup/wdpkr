//! Runtime configuration: defaults → file → env vars → CLI flags.
//!
//! Implementation tracks root `SPEC.md` § Configuration.

mod embed;
mod indexer;
mod store;
mod summarizer;

#[cfg(test)]
pub(crate) mod test_helpers;

pub use embed::EmbedConfig;
pub use indexer::IndexerConfig;
pub use store::StoreConfig;
pub use summarizer::SummarizerConfig;

use serde::Deserialize;
use std::str::FromStr;

pub struct Config {
    pub store: StoreConfig,
    pub embed: EmbedConfig,
    pub summarizer: SummarizerConfig,
    pub indexer: IndexerConfig,
}

impl Config {
    /// Resolve config: defaults → `~/.config/megagrep/config.yaml` → env vars.
    /// CLI-flag overrides happen at the call site.
    pub fn new() -> Self {
        Self::from_file(FileConfig::load())
    }

    /// Build from an explicit (possibly absent) on-disk config — primarily
    /// for tests, where we want to bypass `~/.config/megagrep/config.yaml`.
    pub fn from_file(file: Option<FileConfig>) -> Self {
        Self {
            store: StoreConfig::from_env(&file),
            embed: EmbedConfig::from_env(&file),
            summarizer: SummarizerConfig::from_env(&file),
            indexer: IndexerConfig::from_env(&file),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::new()
    }
}

/// Read an environment variable, parse it, or fall back to `default`.
/// Silent fallback — parse failures use the default. Matches the Dayforward
/// `env_or` pattern referenced in root SPEC § Configuration.
pub(crate) fn env_or<T: FromStr>(key: &str, default: T) -> T {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Use the config-file value if present, otherwise the hardcoded default.
pub(crate) fn file_or<T>(file_val: Option<T>, default: T) -> T {
    file_val.unwrap_or(default)
}

#[derive(Deserialize, Default)]
pub struct FileConfig {
    pub store: Option<FileStoreConfig>,
    pub embedder: Option<FileEmbedConfig>,
    pub summarizer: Option<FileSummarizerConfig>,
    pub indexer: Option<FileIndexerConfig>,
}

#[derive(Deserialize, Default)]
pub struct FileStoreConfig {
    pub provider: Option<String>,
}

#[derive(Deserialize, Default)]
pub struct FileEmbedConfig {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub batch_size: Option<usize>,
    pub ollama_host: Option<String>,
}

#[derive(Deserialize, Default)]
pub struct FileSummarizerConfig {
    pub provider: Option<String>,
    pub model: Option<String>,
}

#[derive(Deserialize, Default)]
pub struct FileIndexerConfig {
    pub namespace: Option<String>,
    pub default_branch: Option<String>,
    pub concurrency: Option<usize>,
    pub max_cost: Option<f64>,
    pub hwm_success_threshold: Option<f64>,
}

impl FileConfig {
    /// Load `~/.config/megagrep/config.yaml`. Returns `None` if the file is
    /// missing or malformed (silent fallback per root SPEC).
    pub fn load() -> Option<Self> {
        let path = dirs::config_dir()?.join("megagrep/config.yaml");
        let content = std::fs::read_to_string(&path).ok()?;
        serde_yaml::from_str(&content).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::test_helpers::{remove_env, remove_envs, set_env};
    use super::*;
    use serial_test::serial;

    fn clear_megagrep_env() {
        remove_envs(&[
            "MEGAGREP_STORE_PROVIDER",
            "MEGAGREP_EMBED_PROVIDER",
            "MEGAGREP_EMBED_MODEL",
            "MEGAGREP_EMBED_BATCH_SIZE",
            "MEGAGREP_SUMMARIZER_PROVIDER",
            "MEGAGREP_SUMMARIZER_MODEL",
            "MEGAGREP_NAMESPACE",
            "MEGAGREP_DEFAULT_BRANCH",
            "MEGAGREP_CONCURRENCY",
            "MEGAGREP_MAX_COST",
            "MEGAGREP_HWM_SUCCESS_THRESHOLD",
            "TURBOPUFFER_API_KEY",
            "VOYAGE_API_KEY",
            "OPENAI_API_KEY",
            "ANTHROPIC_API_KEY",
            "OLLAMA_HOST",
        ]);
    }

    #[test]
    #[serial]
    fn config_assembles_with_all_defaults() {
        clear_megagrep_env();
        let cfg = Config::from_file(None);
        assert_eq!(cfg.store.provider, "turbopuffer");
        assert_eq!(cfg.embed.provider, "voyage");
        assert_eq!(cfg.embed.model, "voyage-code-3");
        assert_eq!(cfg.summarizer.provider, "anthropic");
        assert_eq!(cfg.indexer.concurrency, 8);
    }

    #[test]
    #[serial]
    fn env_or_uses_env_when_set() {
        set_env("__MEGAGREP_TEST_ENV_OR_KEY", "42");
        let v: u32 = env_or("__MEGAGREP_TEST_ENV_OR_KEY", 1);
        assert_eq!(v, 42);
        remove_env("__MEGAGREP_TEST_ENV_OR_KEY");
    }

    #[test]
    #[serial]
    fn env_or_falls_back_when_unset() {
        remove_env("__MEGAGREP_TEST_ENV_OR_UNSET");
        let v: u32 = env_or("__MEGAGREP_TEST_ENV_OR_UNSET", 7);
        assert_eq!(v, 7);
    }

    #[test]
    #[serial]
    fn env_or_falls_back_when_unparseable() {
        set_env("__MEGAGREP_TEST_ENV_OR_BAD", "not_a_number");
        let v: u32 = env_or("__MEGAGREP_TEST_ENV_OR_BAD", 99);
        assert_eq!(v, 99);
        remove_env("__MEGAGREP_TEST_ENV_OR_BAD");
    }

    #[test]
    fn file_or_prefers_file_value() {
        let result: u32 = file_or(Some(50), 10);
        assert_eq!(result, 50);
    }

    #[test]
    fn file_or_falls_back_when_none() {
        let result: u32 = file_or(None::<u32>, 10);
        assert_eq!(result, 10);
    }

    #[test]
    #[serial]
    fn file_values_used_when_env_absent() {
        clear_megagrep_env();
        let file = FileConfig {
            indexer: Some(FileIndexerConfig {
                concurrency: Some(32),
                ..Default::default()
            }),
            ..Default::default()
        };
        let cfg = Config::from_file(Some(file));
        assert_eq!(cfg.indexer.concurrency, 32);
    }

    #[test]
    #[serial]
    fn env_overrides_file_values() {
        clear_megagrep_env();
        set_env("MEGAGREP_CONCURRENCY", "64");
        let file = FileConfig {
            indexer: Some(FileIndexerConfig {
                concurrency: Some(32),
                ..Default::default()
            }),
            ..Default::default()
        };
        let cfg = Config::from_file(Some(file));
        assert_eq!(cfg.indexer.concurrency, 64);
        clear_megagrep_env();
    }

    #[test]
    fn file_config_yaml_round_trip() {
        let yaml = r#"
store:
  provider: turbopuffer
embedder:
  provider: ollama
  model: nomic-embed-text
  batch_size: 32
summarizer:
  provider: anthropic
  model: claude-haiku-4-5-20251001
indexer:
  default_branch: main
  concurrency: 16
  max_cost: 75
  hwm_success_threshold: 0.9
"#;
        let parsed: FileConfig = serde_yaml::from_str(yaml).expect("yaml parses");
        assert_eq!(
            parsed.embedder.as_ref().unwrap().provider.as_deref(),
            Some("ollama")
        );
        assert_eq!(parsed.indexer.as_ref().unwrap().concurrency, Some(16));
    }
}
