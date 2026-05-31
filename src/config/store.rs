use anyhow::Result;
use std::path::PathBuf;

use super::{FileConfig, Resolved, Source, env_or_resolved, file_or_resolved};

/// Resolved store configuration. Provider-specific settings live in nested
/// sub-structs (one per backend) rather than a flat `{provider}_field`
/// namespace, so adding a backend doesn't pollute a shared field list.
pub struct StoreConfig {
    pub provider: String,
    pub turbopuffer: TurbopufferConfig,
    pub duckdb: DuckdbConfig,
}

/// Turbopuffer backend settings.
pub struct TurbopufferConfig {
    pub api_key: String,
}

/// DuckDB (local, file-backed) backend settings.
pub struct DuckdbConfig {
    /// Path to the on-disk DuckDB database file.
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
    pub duckdb: DuckdbSources,
}

#[derive(Debug, Clone)]
pub struct TurbopufferSources {
    pub api_key: Source,
}

#[derive(Debug, Clone)]
pub struct DuckdbSources {
    pub path: Source,
}

/// Default DuckDB database path: `$XDG_DATA_HOME/wdpkr/wdpkr.duckdb`, falling
/// back to `~/.local/share/wdpkr/wdpkr.duckdb`. Mirrors the uniform-XDG
/// approach used for the config file path (see [`super::FileConfig::path`]).
pub fn default_duckdb_path() -> String {
    let base = if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
        PathBuf::from(xdg)
    } else if let Some(home) = dirs::home_dir() {
        home.join(".local").join("share")
    } else {
        PathBuf::from(".")
    };
    base.join("wdpkr")
        .join("wdpkr.duckdb")
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

        let duckdb_path: Resolved<String> = env_or_resolved(
            "WDPKR_DUCKDB_PATH",
            file_or_resolved(
                f.and_then(|s| s.duckdb.as_ref().and_then(|d| d.path.clone())),
                default_duckdb_path(),
            ),
        );

        (
            Self {
                provider: provider.value,
                turbopuffer: TurbopufferConfig {
                    api_key: api_key.value,
                },
                duckdb: DuckdbConfig {
                    path: duckdb_path.value,
                },
            },
            StoreSources {
                provider: provider.source,
                turbopuffer: TurbopufferSources {
                    api_key: api_key.source,
                },
                duckdb: DuckdbSources {
                    path: duckdb_path.source,
                },
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::test_helpers::{remove_envs, set_env};
    use crate::config::{FileDuckdbConfig, FileStoreConfig, FileTurbopufferConfig};
    use serial_test::serial;

    fn clear_env() {
        remove_envs(&[
            "WDPKR_STORE_PROVIDER",
            "TURBOPUFFER_API_KEY",
            "WDPKR_DUCKDB_PATH",
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
            duckdb: DuckdbConfig {
                path: "/tmp/wdpkr-test.duckdb".into(),
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
        assert!(cfg.duckdb.path.ends_with("wdpkr/wdpkr.duckdb"));
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

    // ── DuckDB path ────────────────────────────────────────────────────

    #[test]
    #[serial]
    fn duckdb_path_default_honors_xdg_data_home() {
        clear_env();
        set_env("XDG_DATA_HOME", "/tmp/wdpkr-xdg-data");
        let cfg = StoreConfig::from_env(&None);
        assert_eq!(cfg.duckdb.path, "/tmp/wdpkr-xdg-data/wdpkr/wdpkr.duckdb");
        clear_env();
    }

    #[test]
    #[serial]
    fn duckdb_path_env_override() {
        clear_env();
        set_env("WDPKR_DUCKDB_PATH", "/custom/db.duckdb");
        let cfg = StoreConfig::from_env(&None);
        assert_eq!(cfg.duckdb.path, "/custom/db.duckdb");
        clear_env();
    }

    #[test]
    #[serial]
    fn duckdb_path_from_file() {
        clear_env();
        let file = FileConfig {
            store: Some(FileStoreConfig {
                duckdb: Some(FileDuckdbConfig {
                    path: Some("/file/db.duckdb".into()),
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let cfg = StoreConfig::from_env(&Some(file));
        assert_eq!(cfg.duckdb.path, "/file/db.duckdb");
    }

    // ── Source attribution ────────────────────────────────────────────

    #[test]
    #[serial]
    fn resolve_marks_default_when_no_input() {
        clear_env();
        let (_, sources) = StoreConfig::resolve(&None);
        assert_eq!(sources.provider, Source::Default);
        assert_eq!(sources.turbopuffer.api_key, Source::Default);
        assert_eq!(sources.duckdb.path, Source::Default);
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
        set_env("WDPKR_DUCKDB_PATH", "/x.duckdb");
        let (_, sources) = StoreConfig::resolve(&None);
        assert_eq!(sources.provider, Source::Env("WDPKR_STORE_PROVIDER"));
        assert_eq!(
            sources.turbopuffer.api_key,
            Source::Env("TURBOPUFFER_API_KEY")
        );
        assert_eq!(sources.duckdb.path, Source::Env("WDPKR_DUCKDB_PATH"));
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
    #[cfg(feature = "duckdb")]
    fn validate_passes_duckdb_with_path() {
        assert!(cfg("duckdb", "").validate().is_ok());
    }
}
