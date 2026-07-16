//! Runtime configuration: defaults → file → env vars → CLI flags.
//!
//! Implementation tracks root `SPEC.md` § Configuration. Two API surfaces:
//!
//! - [`Config::new`] — convenience for code that just needs values.
//! - [`ResolvedConfig::new`] — values **plus** per-field source attribution,
//!   used by `wdpkr config list` / `config get`.
//!
//! ## Validation timing
//!
//! `Config::new()` does **not** call `validate()` on subconfigs. Some
//! subcommands (notably `wdpkr config get`) must work without provider
//! credentials. Validation is the responsibility of entry points that
//! actually hit external APIs (the indexer, the searcher), where missing
//! credentials are a fail-fast condition.
//!
//! ## File location
//!
//! The SPEC anchors on `~/.config/wdpkr/config.yaml` regardless of OS,
//! honoring `$XDG_CONFIG_HOME` if set. We deliberately do NOT use
//! `dirs::config_dir()` here — that returns
//! `~/Library/Application Support/...` on macOS, which contradicts the SPEC.
//!
//! ## Malformed config files
//!
//! A **missing** config file is fine — the user just hasn't run
//! `wdpkr config init`, defaults are used. A **malformed** config file
//! is a hard error: if the user attempted to set config and got it wrong,
//! we cannot trust which values are intended, so we refuse to proceed
//! until the file is corrected (or removed to revert to defaults).

mod embed;
mod indexer;
mod store;
mod summarizer;
pub mod tap;

#[cfg(test)]
pub(crate) mod test_helpers;

pub use embed::{EmbedConfig, EmbedSources};
pub use indexer::{IndexerConfig, IndexerSources};
pub use store::{
    NidusConfig, NidusSources, StoreConfig, StoreSources, TurbopufferConfig, TurbopufferSources,
    default_nidus_path,
};
pub use summarizer::{SummarizerConfig, SummarizerSources};
pub use tap::{DecayConfig, FileTapConfig, TapConfig, TapsSources};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::str::FromStr;

/// Boilerplate `config.yaml` written by `wdpkr config init`.
/// Kept as a sibling file (not a string literal) so it can carry comments
/// and example values for first-run UX.
pub const DEFAULT_CONFIG_YAML: &str = include_str!("default_config.yaml");

// ── Source attribution ────────────────────────────────────────────────────

/// Where each resolved config field came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// Hardcoded fallback — no env var, no file value.
    Default,
    /// Read from `config.yaml`.
    File,
    /// Read from this env var.
    Env(&'static str),
}

impl Source {
    /// Render as a human-friendly suffix, e.g. `default`, `file`, `env: KEY`.
    /// Used by `wdpkr config list`.
    pub fn label(&self) -> String {
        match self {
            Source::Default => "default".to_string(),
            Source::File => "file".to_string(),
            Source::Env(name) => format!("env: {name}"),
        }
    }
}

/// Internal: a resolved value paired with the source that produced it.
#[derive(Debug, Clone)]
pub(crate) struct Resolved<T> {
    pub value: T,
    pub source: Source,
}

/// Build a `Resolved<T>` whose source is `Default`.
pub(crate) fn resolved_default<T>(value: T) -> Resolved<T> {
    Resolved {
        value,
        source: Source::Default,
    }
}

/// Read an env var, parse it, or fall back to the resolved value.
/// Silent fallback on parse failure — chatty logging here would crowd the
/// CLI with noise from misconfigured env vars in CI.
pub(crate) fn env_or_resolved<T: FromStr>(key: &'static str, fallback: Resolved<T>) -> Resolved<T> {
    if let Ok(s) = std::env::var(key)
        && let Ok(v) = s.parse()
    {
        return Resolved {
            value: v,
            source: Source::Env(key),
        };
    }
    fallback
}

/// Wrap an `Option<T>` from the config file as a `Resolved<T>`.
pub(crate) fn file_or_resolved<T>(file_val: Option<T>, default: T) -> Resolved<T> {
    match file_val {
        Some(v) => Resolved {
            value: v,
            source: Source::File,
        },
        None => Resolved {
            value: default,
            source: Source::Default,
        },
    }
}

/// Value-only wrapper around [`env_or_resolved`]. Matches the Dayforward
/// `env_or` pattern referenced in root SPEC § Configuration. Currently
/// unused by the library (every resolution path captures sources), but
/// preserved for callers that want the simpler API and as the
/// SPEC-documented entry point.
#[allow(dead_code)]
pub(crate) fn env_or<T: FromStr>(key: &'static str, default: T) -> T {
    env_or_resolved(key, resolved_default(default)).value
}

/// Value-only wrapper around [`file_or_resolved`]. See [`env_or`].
#[allow(dead_code)]
pub(crate) fn file_or<T>(file_val: Option<T>, default: T) -> T {
    file_or_resolved(file_val, default).value
}

// ── Config / ResolvedConfig / entries ─────────────────────────────────────

/// Top-level resolved values. Use [`ResolvedConfig`] when you also need
/// source attribution (`config list`, `config get`).
pub struct Config {
    pub store: StoreConfig,
    pub embed: EmbedConfig,
    pub summarizer: SummarizerConfig,
    pub indexer: IndexerConfig,
    pub taps: Vec<TapConfig>,
}

/// Per-field source attribution paralleling [`Config`].
pub struct ConfigSources {
    pub store: StoreSources,
    pub embed: EmbedSources,
    pub summarizer: SummarizerSources,
    pub indexer: IndexerSources,
    pub taps: TapsSources,
}

/// Values + sources, returned by [`ResolvedConfig::new`].
pub struct ResolvedConfig {
    pub config: Config,
    pub sources: ConfigSources,
}

/// One row in `wdpkr config list`.
#[derive(Debug, Clone)]
pub struct ConfigEntry {
    pub key: &'static str,
    pub value: String,
    pub source: Source,
}

impl Config {
    /// Resolve config: defaults → `~/.config/wdpkr/config.yaml` → env
    /// vars. CLI flag overrides happen at the call site.
    ///
    /// Returns an error if the config file exists but is malformed —
    /// callers must surface this to the user, who is expected to fix the
    /// file. A missing file is **not** an error.
    pub fn new() -> Result<Self> {
        Ok(ResolvedConfig::new()?.config)
    }

    /// Build from an explicit (possibly absent) on-disk config — primarily
    /// for tests, where we want to bypass the real `~/.config/...` lookup.
    /// Infallible because the caller has already loaded (and validated) the
    /// `FileConfig`.
    pub fn from_file(file: Option<FileConfig>) -> Self {
        ResolvedConfig::from_file(file).config
    }
}

impl ResolvedConfig {
    /// Resolve all four layers and capture sources for every field.
    /// Returns an error if the config file exists but is malformed (see
    /// [`Config::new`] for rationale).
    pub fn new() -> Result<Self> {
        Ok(Self::from_file(FileConfig::load()?))
    }

    /// Build from an explicit (possibly absent) on-disk config. Infallible.
    pub fn from_file(file: Option<FileConfig>) -> Self {
        let (store, store_sources) = StoreConfig::resolve(&file);
        let (embed, embed_sources) = EmbedConfig::resolve(&file);
        let (summarizer, summarizer_sources) = SummarizerConfig::resolve(&file);
        let (indexer, indexer_sources) = IndexerConfig::resolve(&file);
        let (taps, taps_sources) = tap::resolve(&file);
        Self {
            config: Config {
                store,
                embed,
                summarizer,
                indexer,
                taps,
            },
            sources: ConfigSources {
                store: store_sources,
                embed: embed_sources,
                summarizer: summarizer_sources,
                indexer: indexer_sources,
                taps: taps_sources,
            },
        }
    }

    /// Flat list of (key, value, source) for every non-secret field.
    /// Drives `wdpkr config list`.
    ///
    /// API keys are intentionally excluded — they're sensitive and the SPEC's
    /// `config list` example doesn't show them. A future `config doctor`
    /// command can report set/unset status without revealing values.
    pub fn entries(&self) -> Vec<ConfigEntry> {
        let c = &self.config;
        let s = &self.sources;
        vec![
            ConfigEntry {
                key: "store.provider",
                value: c.store.provider.clone(),
                source: s.store.provider.clone(),
            },
            ConfigEntry {
                key: "store.nidus.path",
                value: c.store.nidus.path.clone(),
                source: s.store.nidus.path.clone(),
            },
            ConfigEntry {
                key: "embedder.provider",
                value: c.embed.provider.clone(),
                source: s.embed.provider.clone(),
            },
            ConfigEntry {
                key: "embedder.model",
                value: c.embed.model.clone(),
                source: s.embed.model.clone(),
            },
            ConfigEntry {
                key: "embedder.batch_size",
                value: c.embed.batch_size.to_string(),
                source: s.embed.batch_size.clone(),
            },
            ConfigEntry {
                key: "embedder.embed_mode",
                value: c.embed.embed_mode.clone(),
                source: s.embed.embed_mode.clone(),
            },
            ConfigEntry {
                key: "embedder.ollama_host",
                value: c.embed.ollama_host.clone(),
                source: s.embed.ollama_host.clone(),
            },
            ConfigEntry {
                key: "summarizer.provider",
                value: c.summarizer.provider.clone(),
                source: s.summarizer.provider.clone(),
            },
            ConfigEntry {
                key: "summarizer.model",
                value: c.summarizer.model.clone(),
                source: s.summarizer.model.clone(),
            },
            ConfigEntry {
                key: "indexer.namespace",
                value: c.indexer.namespace.clone(),
                source: s.indexer.namespace.clone(),
            },
            ConfigEntry {
                key: "indexer.default_branch",
                value: c.indexer.default_branch.clone(),
                source: s.indexer.default_branch.clone(),
            },
            ConfigEntry {
                key: "indexer.git_remote",
                value: c.indexer.git_remote.clone(),
                source: s.indexer.git_remote.clone(),
            },
            ConfigEntry {
                key: "indexer.concurrency",
                value: c.indexer.concurrency.to_string(),
                source: s.indexer.concurrency.clone(),
            },
            ConfigEntry {
                key: "indexer.max_cost",
                value: c.indexer.max_cost.to_string(),
                source: s.indexer.max_cost.clone(),
            },
            ConfigEntry {
                key: "indexer.hwm_success_threshold",
                value: c.indexer.hwm_success_threshold.to_string(),
                source: s.indexer.hwm_success_threshold.clone(),
            },
            ConfigEntry {
                key: "taps",
                value: c
                    .taps
                    .iter()
                    .map(|p| p.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                source: s.taps.source.clone(),
            },
        ]
    }

    /// Look up one entry by dotted key. Drives `wdpkr config get`.
    pub fn get(&self, key: &str) -> Option<ConfigEntry> {
        self.entries().into_iter().find(|e| e.key == key)
    }
}

// ── FileConfig: serde target + load/save/set ─────────────────────────────

/// On-disk config schema. Every field optional; missing keys fall back to
/// env vars or hardcoded defaults via the resolution helpers above.
#[derive(Deserialize, Serialize, Default, Debug, Clone)]
pub struct FileConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store: Option<FileStoreConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedder: Option<FileEmbedConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summarizer: Option<FileSummarizerConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indexer: Option<FileIndexerConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub taps: Option<Vec<FileTapConfig>>,
}

#[derive(Deserialize, Serialize, Default, Debug, Clone)]
pub struct FileStoreConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Turbopuffer backend settings (`store.turbopuffer.*`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turbopuffer: Option<FileTurbopufferConfig>,
    /// nidus backend settings (`store.nidus.*`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nidus: Option<FileNidusConfig>,
    /// Deprecated flat alias for `store.turbopuffer.api_key`. Still read for
    /// backwards compatibility; prefer the nested form. Never written by
    /// `config set`/`init`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turbopuffer_api_key: Option<String>,
}

#[derive(Deserialize, Serialize, Default, Debug, Clone)]
pub struct FileTurbopufferConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

#[derive(Deserialize, Serialize, Default, Debug, Clone)]
pub struct FileNidusConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Deserialize, Serialize, Default, Debug, Clone)]
pub struct FileEmbedConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_size: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embed_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ollama_host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voyage_api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openai_api_key: Option<String>,
}

#[derive(Deserialize, Serialize, Default, Debug, Clone)]
pub struct FileSummarizerConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anthropic_api_key: Option<String>,
}

#[derive(Deserialize, Serialize, Default, Debug, Clone)]
pub struct FileIndexerConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_remote: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concurrency: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cost: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hwm_success_threshold: Option<f64>,
}

impl FileConfig {
    /// Resolved path of the config file: `$XDG_CONFIG_HOME/wdpkr/config.yaml`,
    /// or `~/.config/wdpkr/config.yaml` if `XDG_CONFIG_HOME` is unset.
    /// SPEC-anchored — uniform across Linux and macOS.
    pub fn path() -> Result<PathBuf> {
        let base = if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
            PathBuf::from(xdg)
        } else {
            dirs::home_dir()
                .context("could not resolve home directory")?
                .join(".config")
        };
        Ok(base.join("wdpkr").join("config.yaml"))
    }

    /// Load the config file.
    ///
    /// Returns:
    /// - `Ok(None)` — the file does not exist; the caller should use
    ///   defaults (this is the normal path for users who haven't run
    ///   `wdpkr config init`).
    /// - `Ok(Some(cfg))` — file parsed successfully.
    /// - `Err(_)` — the file exists but is unreadable or malformed. The
    ///   user must fix or remove it; we refuse to guess at intended values.
    pub fn load() -> Result<Option<Self>> {
        let path = Self::path()?;
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(anyhow::Error::new(e))
                    .with_context(|| format!("reading {}", path.display()));
            }
        };
        let parsed: Self = serde_yaml::from_str(&content).with_context(|| {
            format!(
                "parsing {}: fix the malformed config or delete it to revert to defaults",
                path.display()
            )
        })?;
        Ok(Some(parsed))
    }

    /// Write to the canonical location. Creates parent dirs as needed and
    /// sets file mode 0600 on Unix per SPEC.
    ///
    /// Note: serde_yaml does not preserve comments. After this call any
    /// hand-written comments in the file are lost. `config init` writes
    /// [`DEFAULT_CONFIG_YAML`] verbatim to preserve its comments on first
    /// run; subsequent `config set` round-trips lose them.
    pub fn save(&self) -> Result<PathBuf> {
        let path = Self::path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let yaml = serde_yaml::to_string(self).context("serializing config to YAML")?;
        std::fs::write(&path, yaml).with_context(|| format!("writing {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                .with_context(|| format!("chmod 0600 {}", path.display()))?;
        }
        Ok(path)
    }

    /// Set a single field by dotted key. Supports the same keys as
    /// [`ResolvedConfig::entries`]. Returns an error for unknown keys or
    /// for values that fail to parse into the field's type.
    ///
    /// Drives `wdpkr config set <key> <value>`.
    pub fn set(&mut self, key: &str, value: &str) -> Result<()> {
        match key {
            "store.provider" => {
                self.store.get_or_insert_default().provider = Some(value.into());
            }
            "embedder.provider" => {
                self.embedder.get_or_insert_default().provider = Some(value.into());
            }
            "embedder.model" => {
                self.embedder.get_or_insert_default().model = Some(value.into());
            }
            "embedder.embed_mode" => {
                self.embedder.get_or_insert_default().embed_mode = Some(value.into());
            }
            "embedder.batch_size" => {
                let parsed: usize = value.parse().with_context(|| {
                    format!("embedder.batch_size: cannot parse '{value}' as usize")
                })?;
                self.embedder.get_or_insert_default().batch_size = Some(parsed);
            }
            "embedder.ollama_host" => {
                self.embedder.get_or_insert_default().ollama_host = Some(value.into());
            }
            "embedder.voyage_api_key" => {
                self.embedder.get_or_insert_default().voyage_api_key = Some(value.into());
            }
            "embedder.openai_api_key" => {
                self.embedder.get_or_insert_default().openai_api_key = Some(value.into());
            }
            "store.turbopuffer.api_key" => {
                self.store
                    .get_or_insert_default()
                    .turbopuffer
                    .get_or_insert_default()
                    .api_key = Some(value.into());
            }
            // Deprecated flat alias — write the nested field so the file stays
            // in the canonical shape.
            "store.turbopuffer_api_key" => {
                self.store
                    .get_or_insert_default()
                    .turbopuffer
                    .get_or_insert_default()
                    .api_key = Some(value.into());
            }
            "store.nidus.path" => {
                self.store
                    .get_or_insert_default()
                    .nidus
                    .get_or_insert_default()
                    .path = Some(value.into());
            }
            "summarizer.provider" => {
                self.summarizer.get_or_insert_default().provider = Some(value.into());
            }
            "summarizer.model" => {
                self.summarizer.get_or_insert_default().model = Some(value.into());
            }
            "summarizer.anthropic_api_key" => {
                self.summarizer.get_or_insert_default().anthropic_api_key = Some(value.into());
            }
            "indexer.namespace" => {
                self.indexer.get_or_insert_default().namespace = Some(value.into());
            }
            "indexer.default_branch" => {
                self.indexer.get_or_insert_default().default_branch = Some(value.into());
            }
            "indexer.git_remote" => {
                self.indexer.get_or_insert_default().git_remote = Some(value.into());
            }
            "indexer.concurrency" => {
                let parsed: usize = value.parse().with_context(|| {
                    format!("indexer.concurrency: cannot parse '{value}' as usize")
                })?;
                self.indexer.get_or_insert_default().concurrency = Some(parsed);
            }
            "indexer.max_cost" => {
                let parsed: f64 = value
                    .parse()
                    .with_context(|| format!("indexer.max_cost: cannot parse '{value}' as f64"))?;
                self.indexer.get_or_insert_default().max_cost = Some(parsed);
            }
            "indexer.hwm_success_threshold" => {
                let parsed: f64 = value.parse().with_context(|| {
                    format!("indexer.hwm_success_threshold: cannot parse '{value}' as f64")
                })?;
                self.indexer.get_or_insert_default().hwm_success_threshold = Some(parsed);
            }
            other => bail!("unknown config key: {other}"),
        }
        Ok(())
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::test_helpers::{remove_env, remove_envs, set_env};
    use super::*;
    use serial_test::serial;

    fn clear_wdpkr_env() {
        remove_envs(&[
            "WDPKR_STORE_PROVIDER",
            "WDPKR_EMBED_PROVIDER",
            "WDPKR_EMBED_MODEL",
            "WDPKR_EMBED_BATCH_SIZE",
            "WDPKR_SUMMARIZER_PROVIDER",
            "WDPKR_SUMMARIZER_MODEL",
            "WDPKR_NAMESPACE",
            "WDPKR_DEFAULT_BRANCH",
            "WDPKR_CONCURRENCY",
            "WDPKR_MAX_COST",
            "WDPKR_HWM_SUCCESS_THRESHOLD",
            "TURBOPUFFER_API_KEY",
            "VOYAGE_API_KEY",
            "OPENAI_API_KEY",
            "ANTHROPIC_API_KEY",
            "OLLAMA_HOST",
            "XDG_CONFIG_HOME",
        ]);
    }

    /// Build a unique tempdir path per-test, avoiding collisions in
    /// parallel test runs. The dir is created lazily by callers.
    fn unique_tempdir(label: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("wdpkr-{label}-{}-{nanos}", std::process::id()))
    }

    // ── Existing coverage (preserved) ─────────────────────────────────

    #[test]
    #[serial]
    fn config_assembles_with_all_defaults() {
        clear_wdpkr_env();
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
        set_env("__WDPKR_TEST_ENV_OR_KEY", "42");
        let v: u32 = env_or("__WDPKR_TEST_ENV_OR_KEY", 1);
        assert_eq!(v, 42);
        remove_env("__WDPKR_TEST_ENV_OR_KEY");
    }

    #[test]
    #[serial]
    fn env_or_falls_back_when_unset() {
        remove_env("__WDPKR_TEST_ENV_OR_UNSET");
        let v: u32 = env_or("__WDPKR_TEST_ENV_OR_UNSET", 7);
        assert_eq!(v, 7);
    }

    #[test]
    #[serial]
    fn env_or_falls_back_when_unparseable() {
        set_env("__WDPKR_TEST_ENV_OR_BAD", "not_a_number");
        let v: u32 = env_or("__WDPKR_TEST_ENV_OR_BAD", 99);
        assert_eq!(v, 99);
        remove_env("__WDPKR_TEST_ENV_OR_BAD");
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
        clear_wdpkr_env();
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
        clear_wdpkr_env();
        set_env("WDPKR_CONCURRENCY", "64");
        let file = FileConfig {
            indexer: Some(FileIndexerConfig {
                concurrency: Some(32),
                ..Default::default()
            }),
            ..Default::default()
        };
        let cfg = Config::from_file(Some(file));
        assert_eq!(cfg.indexer.concurrency, 64);
        clear_wdpkr_env();
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

    // ── Source attribution ────────────────────────────────────────────

    #[test]
    #[serial]
    fn source_default_when_no_env_no_file() {
        clear_wdpkr_env();
        let resolved = ResolvedConfig::from_file(None);
        assert_eq!(resolved.sources.indexer.concurrency, Source::Default);
        assert_eq!(resolved.sources.embed.provider, Source::Default);
    }

    #[test]
    #[serial]
    fn source_file_when_only_file_provides() {
        clear_wdpkr_env();
        let file = FileConfig {
            indexer: Some(FileIndexerConfig {
                concurrency: Some(24),
                ..Default::default()
            }),
            ..Default::default()
        };
        let resolved = ResolvedConfig::from_file(Some(file));
        assert_eq!(resolved.sources.indexer.concurrency, Source::File);
    }

    #[test]
    #[serial]
    fn source_env_when_env_overrides_file() {
        clear_wdpkr_env();
        set_env("WDPKR_CONCURRENCY", "40");
        let file = FileConfig {
            indexer: Some(FileIndexerConfig {
                concurrency: Some(24),
                ..Default::default()
            }),
            ..Default::default()
        };
        let resolved = ResolvedConfig::from_file(Some(file));
        assert_eq!(
            resolved.sources.indexer.concurrency,
            Source::Env("WDPKR_CONCURRENCY")
        );
        clear_wdpkr_env();
    }

    #[test]
    fn source_label_renders_correctly() {
        assert_eq!(Source::Default.label(), "default");
        assert_eq!(Source::File.label(), "file");
        assert_eq!(Source::Env("WDPKR_FOO").label(), "env: WDPKR_FOO");
    }

    // ── entries / get ─────────────────────────────────────────────────

    #[test]
    #[serial]
    fn entries_lists_every_non_secret_field() {
        clear_wdpkr_env();
        let resolved = ResolvedConfig::from_file(None);
        let entries = resolved.entries();
        let keys: Vec<&str> = entries.iter().map(|e| e.key).collect();
        for required in [
            "store.provider",
            "embedder.provider",
            "embedder.model",
            "embedder.batch_size",
            "embedder.embed_mode",
            "embedder.ollama_host",
            "summarizer.provider",
            "summarizer.model",
            "indexer.namespace",
            "indexer.default_branch",
            "indexer.concurrency",
            "indexer.max_cost",
            "indexer.hwm_success_threshold",
        ] {
            assert!(keys.contains(&required), "missing entry: {required}");
        }
        // Secrets explicitly excluded.
        assert!(!keys.iter().any(|k| k.contains("api_key")));
    }

    #[test]
    #[serial]
    fn entries_carry_source_for_each_field() {
        clear_wdpkr_env();
        set_env("WDPKR_EMBED_MODEL", "voyage-3-large");
        let resolved = ResolvedConfig::from_file(None);
        let model = resolved
            .entries()
            .into_iter()
            .find(|e| e.key == "embedder.model")
            .expect("embedder.model entry");
        assert_eq!(model.value, "voyage-3-large");
        assert_eq!(model.source, Source::Env("WDPKR_EMBED_MODEL"));
        clear_wdpkr_env();
    }

    #[test]
    #[serial]
    fn get_returns_one_entry() {
        clear_wdpkr_env();
        let resolved = ResolvedConfig::from_file(None);
        let e = resolved.get("indexer.concurrency").expect("entry exists");
        assert_eq!(e.value, "8");
        assert_eq!(e.source, Source::Default);
    }

    #[test]
    #[serial]
    fn get_returns_none_for_unknown_key() {
        clear_wdpkr_env();
        let resolved = ResolvedConfig::from_file(None);
        assert!(resolved.get("totally.bogus.key").is_none());
    }

    // ── FileConfig::set ───────────────────────────────────────────────

    #[test]
    fn set_string_field() {
        let mut file = FileConfig::default();
        file.set("embedder.model", "voyage-3-large").unwrap();
        assert_eq!(file.embedder.unwrap().model.unwrap(), "voyage-3-large");
    }

    #[test]
    fn set_integer_field() {
        let mut file = FileConfig::default();
        file.set("indexer.concurrency", "32").unwrap();
        assert_eq!(file.indexer.unwrap().concurrency, Some(32));
    }

    #[test]
    fn set_float_field() {
        let mut file = FileConfig::default();
        file.set("indexer.hwm_success_threshold", "0.85").unwrap();
        assert!((file.indexer.unwrap().hwm_success_threshold.unwrap() - 0.85).abs() < f64::EPSILON);
    }

    #[test]
    fn set_unknown_key_errors() {
        let mut file = FileConfig::default();
        let err = file.set("nope.bogus", "x").unwrap_err();
        assert!(err.to_string().contains("unknown config key"));
    }

    #[test]
    fn set_invalid_value_errors() {
        let mut file = FileConfig::default();
        let err = file.set("indexer.concurrency", "not-a-number").unwrap_err();
        assert!(err.to_string().contains("indexer.concurrency"));
    }

    #[test]
    fn set_nested_store_keys() {
        let mut file = FileConfig::default();
        file.set("store.nidus.path", "/tmp/nidus").unwrap();
        file.set("store.turbopuffer.api_key", "tp-key").unwrap();
        let store = file.store.as_ref().unwrap();
        assert_eq!(
            store.nidus.as_ref().unwrap().path.as_deref(),
            Some("/tmp/nidus")
        );
        assert_eq!(
            store.turbopuffer.as_ref().unwrap().api_key.as_deref(),
            Some("tp-key")
        );
    }

    #[test]
    fn set_deprecated_flat_key_writes_nested() {
        // The deprecated `store.turbopuffer_api_key` alias writes the nested
        // field so the file lands in the canonical shape.
        let mut file = FileConfig::default();
        file.set("store.turbopuffer_api_key", "legacy-key").unwrap();
        let store = file.store.as_ref().unwrap();
        assert_eq!(
            store.turbopuffer.as_ref().unwrap().api_key.as_deref(),
            Some("legacy-key")
        );
        assert!(store.turbopuffer_api_key.is_none());
    }

    #[test]
    fn set_does_not_clobber_other_subconfigs() {
        let mut file = FileConfig::default();
        file.set("embedder.model", "voyage-3-large").unwrap();
        file.set("indexer.concurrency", "16").unwrap();
        // Both values land on the right subconfigs without overwriting each
        // other (get_or_insert_default semantics).
        assert_eq!(
            file.embedder.as_ref().unwrap().model.as_deref(),
            Some("voyage-3-large")
        );
        assert_eq!(file.indexer.as_ref().unwrap().concurrency, Some(16));
    }

    // ── DEFAULT_CONFIG_YAML ───────────────────────────────────────────

    #[test]
    fn default_yaml_is_valid_and_parses() {
        let parsed: FileConfig =
            serde_yaml::from_str(DEFAULT_CONFIG_YAML).expect("default yaml parses");
        assert_eq!(
            parsed.store.as_ref().unwrap().provider.as_deref(),
            Some("turbopuffer")
        );
        assert_eq!(
            parsed.embedder.as_ref().unwrap().provider.as_deref(),
            Some("voyage")
        );
        assert_eq!(parsed.indexer.as_ref().unwrap().concurrency, Some(8));
    }

    // ── path / load / save ────────────────────────────────────────────

    #[test]
    #[serial]
    fn path_respects_xdg_config_home() {
        clear_wdpkr_env();
        set_env("XDG_CONFIG_HOME", "/tmp/wdpkr-xdg-test");
        let p = FileConfig::path().unwrap();
        assert_eq!(p, PathBuf::from("/tmp/wdpkr-xdg-test/wdpkr/config.yaml"));
        clear_wdpkr_env();
    }

    #[test]
    #[serial]
    fn save_then_load_round_trip() {
        clear_wdpkr_env();
        let tmp = unique_tempdir("save-load");
        std::fs::create_dir_all(&tmp).unwrap();
        set_env("XDG_CONFIG_HOME", tmp.to_str().unwrap());

        let mut file = FileConfig::default();
        file.set("indexer.concurrency", "12").unwrap();
        file.set("embedder.model", "voyage-3-large").unwrap();
        let written = file.save().unwrap();
        assert!(written.starts_with(&tmp));

        let loaded = FileConfig::load()
            .expect("load is Ok")
            .expect("file present");
        assert_eq!(loaded.indexer.as_ref().unwrap().concurrency, Some(12));
        assert_eq!(
            loaded.embedder.as_ref().unwrap().model.as_deref(),
            Some("voyage-3-large")
        );

        std::fs::remove_dir_all(&tmp).ok();
        clear_wdpkr_env();
    }

    #[test]
    #[serial]
    #[cfg(unix)]
    fn save_uses_0600_permissions() {
        use std::os::unix::fs::PermissionsExt;
        clear_wdpkr_env();
        let tmp = unique_tempdir("perms");
        std::fs::create_dir_all(&tmp).unwrap();
        set_env("XDG_CONFIG_HOME", tmp.to_str().unwrap());

        let path = FileConfig::default().save().unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);

        std::fs::remove_dir_all(&tmp).ok();
        clear_wdpkr_env();
    }

    #[test]
    #[serial]
    fn load_returns_ok_none_for_missing_file() {
        clear_wdpkr_env();
        let tmp = unique_tempdir("missing");
        // Note: do NOT create the dir — file should be missing.
        set_env("XDG_CONFIG_HOME", tmp.to_str().unwrap());
        // Missing file is the normal "no config yet" case — not an error.
        let loaded = FileConfig::load().expect("missing file is Ok(None), not Err");
        assert!(loaded.is_none());
        clear_wdpkr_env();
    }

    #[test]
    #[serial]
    fn load_errors_on_malformed_yaml() {
        clear_wdpkr_env();
        let tmp = unique_tempdir("malformed");
        std::fs::create_dir_all(tmp.join("wdpkr")).unwrap();
        std::fs::write(tmp.join("wdpkr/config.yaml"), "not: : valid: yaml: [").unwrap();
        set_env("XDG_CONFIG_HOME", tmp.to_str().unwrap());

        // Hard error per design: a malformed file means the user attempted
        // to set config and got it wrong. Refuse to proceed until fixed.
        let err = FileConfig::load().expect_err("malformed yaml must error");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("malformed config") || msg.contains("parsing"),
            "error message should mention parsing/malformed; got: {msg}"
        );
        // The path of the offending file should appear so the user knows
        // exactly what to fix.
        assert!(
            msg.contains("config.yaml"),
            "error must name the file: {msg}"
        );

        std::fs::remove_dir_all(&tmp).ok();
        clear_wdpkr_env();
    }

    #[test]
    #[serial]
    fn config_new_propagates_malformed_error() {
        clear_wdpkr_env();
        let tmp = unique_tempdir("propagate");
        std::fs::create_dir_all(tmp.join("wdpkr")).unwrap();
        std::fs::write(tmp.join("wdpkr/config.yaml"), "not: : valid: yaml: [").unwrap();
        set_env("XDG_CONFIG_HOME", tmp.to_str().unwrap());

        // Both top-level constructors must surface the load error so the
        // CLI can render it and exit non-zero.
        assert!(Config::new().is_err());
        assert!(ResolvedConfig::new().is_err());

        std::fs::remove_dir_all(&tmp).ok();
        clear_wdpkr_env();
    }
}
