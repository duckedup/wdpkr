use anyhow::Result;
use std::path::PathBuf;

use super::{FileConfig, Resolved, Source, env_or_resolved, file_or_resolved};

/// Resolved store configuration. Provider-specific settings live in nested
/// sub-structs (one per backend) rather than a flat `{provider}_field`
/// namespace, so adding a backend doesn't pollute a shared field list.
pub struct StoreConfig {
    pub provider: String,
    pub turbopuffer: TurbopufferConfig,
    pub nidus: NidusConfig,
}

/// Turbopuffer backend settings.
pub struct TurbopufferConfig {
    pub api_key: String,
}

/// nidus (local, file-backed, pure-Rust) backend settings.
pub struct NidusConfig {
    /// Path to the on-disk nidus store **directory**.
    pub path: String,
}

impl StoreConfig {
    pub fn validate(&self) -> Result<()> {
        let provider = crate::store::resolve_provider(&self.provider)?;
        provider.validate(self)
    }
}

/// Per-field source attribution paralleling [`StoreConfig`].
#[derive(Debug, Clone)]
pub struct StoreSources {
    pub provider: Source,
    pub turbopuffer: TurbopufferSources,
    pub nidus: NidusSources,
}

#[derive(Debug, Clone)]
pub struct TurbopufferSources {
    pub api_key: Source,
}

#[derive(Debug, Clone)]
pub struct NidusSources {
    pub path: Source,
}

/// Default nidus store directory: `$XDG_DATA_HOME/wdpkr/nidus`, falling back to
/// `~/.local/share/wdpkr/nidus`. Mirrors the uniform-XDG approach used for the
/// config file path (see [`super::FileConfig::path`]).
pub fn default_nidus_path() -> String {
    let base = if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
        PathBuf::from(xdg)
    } else if let Some(home) = dirs::home_dir() {
        home.join(".local").join("share")
    } else {
        PathBuf::from(".")
    };
    base.join("wdpkr")
        .join("nidus")
        .to_string_lossy()
        .into_owned()
}

impl StoreConfig {
    /// Convenience: drop the source map.
    pub fn from_env(file: &Option<FileConfig>) -> Self {
        Self::resolve(file).0
    }

    /// Canonical resolver — returns values + per-field sources.
    pub fn resolve(file: &Option<FileConfig>) -> (Self, StoreSources) {
        let f = file.as_ref().and_then(|f| f.store.as_ref());

        let provider: Resolved<String> = env_or_resolved(
            "WDPKR_STORE_PROVIDER",
            file_or_resolved(f.and_then(|s| s.provider.clone()), "turbopuffer".into()),
        );

        // Prefer the nested `store.turbopuffer.api_key`; fall back to the
        // deprecated flat `store.turbopuffer_api_key` for backwards compat.
        let tpuf_file_key = f
            .and_then(|s| s.turbopuffer.as_ref().and_then(|t| t.api_key.clone()))
            .or_else(|| f.and_then(|s| s.turbopuffer_api_key.clone()));
        let api_key: Resolved<String> = env_or_resolved(
            "TURBOPUFFER_API_KEY",
            file_or_resolved(tpuf_file_key, String::new()),
        );

        let nidus_path: Resolved<String> = env_or_resolved(
            "WDPKR_NIDUS_PATH",
            file_or_resolved(
                f.and_then(|s| s.nidus.as_ref().and_then(|d| d.path.clone())),
                default_nidus_path(),
            ),
        );

        (
            Self {
                provider: provider.value,
                turbopuffer: TurbopufferConfig {
                    api_key: api_key.value,
                },
                nidus: NidusConfig {
                    path: nidus_path.value,
                },
            },
            StoreSources {
                provider: provider.source,
                turbopuffer: TurbopufferSources {
                    api_key: api_key.source,
                },
                nidus: NidusSources {
                    path: nidus_path.source,
                },
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::test_helpers::{remove_envs, set_env};
    use crate::config::{FileNidusConfig, FileStoreConfig, FileTurbopufferConfig};
    use serial_test::serial;

    fn clear_env() {
        remove_envs(&[
            "WDPKR_STORE_PROVIDER",
            "TURBOPUFFER_API_KEY",
            "WDPKR_NIDUS_PATH",
            "XDG_DATA_HOME",
        ]);
    }

    /// Construct a resolved `StoreConfig` for validation tests.
    fn cfg(provider: &str, api_key: &str) -> StoreConfig {
        StoreConfig {
            provider: provider.into(),
            turbopuffer: TurbopufferConfig {
                api_key: api_key.into(),
            },
            nidus: NidusConfig {
                path: "/tmp/wdpkr-test-nidus".into(),
            },
        }
    }

    #[test]
    #[serial]
    fn defaults() {
        clear_env();
        let cfg = StoreConfig::from_env(&None);
        assert_eq!(cfg.provider, "turbopuffer");
        assert_eq!(cfg.turbopuffer.api_key, "");
        assert!(cfg.nidus.path.ends_with("wdpkr/nidus"));
    }

    #[test]
    #[serial]
    fn env_overrides_provider() {
        clear_env();
        set_env("WDPKR_STORE_PROVIDER", "qdrant");
        let cfg = StoreConfig::from_env(&None);
        assert_eq!(cfg.provider, "qdrant");
        clear_env();
    }

    #[test]
    #[serial]
    fn env_provides_api_key() {
        clear_env();
        set_env("TURBOPUFFER_API_KEY", "test-key-123");
        let cfg = StoreConfig::from_env(&None);
        assert_eq!(cfg.turbopuffer.api_key, "test-key-123");
        clear_env();
    }

    #[test]
    #[serial]
    fn file_value_used_when_env_absent() {
        clear_env();
        let file = FileConfig {
            store: Some(FileStoreConfig {
                provider: Some("milvus".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let cfg = StoreConfig::from_env(&Some(file));
        assert_eq!(cfg.provider, "milvus");
    }

    #[test]
    #[serial]
    fn env_beats_file() {
        clear_env();
        set_env("WDPKR_STORE_PROVIDER", "qdrant");
        let file = FileConfig {
            store: Some(FileStoreConfig {
                provider: Some("milvus".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let cfg = StoreConfig::from_env(&Some(file));
        assert_eq!(cfg.provider, "qdrant");
        clear_env();
    }

    // ── Nested vs deprecated-flat api_key ──────────────────────────────

    #[test]
    #[serial]
    fn nested_turbopuffer_api_key_used() {
        clear_env();
        let file = FileConfig {
            store: Some(FileStoreConfig {
                turbopuffer: Some(FileTurbopufferConfig {
                    api_key: Some("nested-key".into()),
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let cfg = StoreConfig::from_env(&Some(file));
        assert_eq!(cfg.turbopuffer.api_key, "nested-key");
    }

    #[test]
    #[serial]
    fn deprecated_flat_api_key_still_read() {
        clear_env();
        let file = FileConfig {
            store: Some(FileStoreConfig {
                turbopuffer_api_key: Some("flat-key".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let cfg = StoreConfig::from_env(&Some(file));
        assert_eq!(cfg.turbopuffer.api_key, "flat-key");
    }

    #[test]
    #[serial]
    fn nested_api_key_beats_deprecated_flat() {
        clear_env();
        let file = FileConfig {
            store: Some(FileStoreConfig {
                turbopuffer: Some(FileTurbopufferConfig {
                    api_key: Some("nested-key".into()),
                }),
                turbopuffer_api_key: Some("flat-key".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let cfg = StoreConfig::from_env(&Some(file));
        assert_eq!(cfg.turbopuffer.api_key, "nested-key");
    }

    // ── nidus path ─────────────────────────────────────────────────────

    #[test]
    #[serial]
    fn nidus_path_default_honors_xdg_data_home() {
        clear_env();
        set_env("XDG_DATA_HOME", "/tmp/wdpkr-xdg-data");
        let cfg = StoreConfig::from_env(&None);
        assert_eq!(cfg.nidus.path, "/tmp/wdpkr-xdg-data/wdpkr/nidus");
        clear_env();
    }

    #[test]
    #[serial]
    fn nidus_path_env_override() {
        clear_env();
        set_env("WDPKR_NIDUS_PATH", "/custom/nidus");
        let cfg = StoreConfig::from_env(&None);
        assert_eq!(cfg.nidus.path, "/custom/nidus");
        clear_env();
    }

    #[test]
    #[serial]
    fn nidus_path_from_file() {
        clear_env();
        let file = FileConfig {
            store: Some(FileStoreConfig {
                nidus: Some(FileNidusConfig {
                    path: Some("/file/nidus".into()),
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let cfg = StoreConfig::from_env(&Some(file));
        assert_eq!(cfg.nidus.path, "/file/nidus");
    }

    // ── Source attribution ────────────────────────────────────────────

    #[test]
    #[serial]
    fn resolve_marks_default_when_no_input() {
        clear_env();
        let (_, sources) = StoreConfig::resolve(&None);
        assert_eq!(sources.provider, Source::Default);
        assert_eq!(sources.turbopuffer.api_key, Source::Default);
        assert_eq!(sources.nidus.path, Source::Default);
    }

    #[test]
    #[serial]
    fn resolve_marks_file_when_file_only() {
        clear_env();
        let file = FileConfig {
            store: Some(FileStoreConfig {
                provider: Some("milvus".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let (_, sources) = StoreConfig::resolve(&Some(file));
        assert_eq!(sources.provider, Source::File);
    }

    #[test]
    #[serial]
    fn resolve_marks_env_when_env_set() {
        clear_env();
        set_env("WDPKR_STORE_PROVIDER", "qdrant");
        set_env("TURBOPUFFER_API_KEY", "key");
        set_env("WDPKR_NIDUS_PATH", "/x-nidus");
        let (_, sources) = StoreConfig::resolve(&None);
        assert_eq!(sources.provider, Source::Env("WDPKR_STORE_PROVIDER"));
        assert_eq!(
            sources.turbopuffer.api_key,
            Source::Env("TURBOPUFFER_API_KEY")
        );
        assert_eq!(sources.nidus.path, Source::Env("WDPKR_NIDUS_PATH"));
        clear_env();
    }

    // ── Validation ───────────────────────────────────────────────────

    #[test]
    fn validate_passes_turbopuffer_with_key() {
        assert!(cfg("turbopuffer", "key-123").validate().is_ok());
    }

    #[test]
    fn validate_fails_turbopuffer_without_key() {
        let err = cfg("turbopuffer", "").validate().unwrap_err();
        assert!(err.to_string().contains("TURBOPUFFER_API_KEY"));
    }

    #[test]
    fn validate_fails_unknown_provider() {
        let err = cfg("qdrant", "key").validate().unwrap_err();
        assert!(err.to_string().contains("unknown store provider"));
    }

    #[test]
    fn validate_passes_nidus_with_path() {
        assert!(cfg("nidus", "").validate().is_ok());
    }
}
