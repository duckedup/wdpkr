use anyhow::Result;

use super::{FileConfig, Resolved, Source, env_or_resolved, file_or_resolved};

pub struct StoreConfig {
    pub provider: String,
    pub turbopuffer: TurbopufferStoreConfig,
    pub duckdb: DuckdbStoreConfig,
}

pub struct TurbopufferStoreConfig {
    pub api_key: String,
}

pub struct DuckdbStoreConfig {
    pub data_path: String,
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
    pub turbopuffer: TurbopufferStoreSources,
    pub duckdb: DuckdbStoreSources,
}

#[derive(Debug, Clone)]
pub struct TurbopufferStoreSources {
    pub api_key: Source,
}

#[derive(Debug, Clone)]
pub struct DuckdbStoreSources {
    pub data_path: Source,
}

fn default_data_path() -> String {
    let base = if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
        std::path::PathBuf::from(xdg)
    } else if let Some(home) = dirs::home_dir() {
        home.join(".local").join("share")
    } else {
        std::path::PathBuf::from(".local/share")
    };
    base.join("wdpkr")
        .join("duckdb")
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

        // Turbopuffer API key: new nested key takes precedence over legacy flat key.
        let nested_api_key = f
            .and_then(|s| s.turbopuffer.as_ref())
            .and_then(|t| t.api_key.clone());
        let legacy_api_key = f.and_then(|s| s.turbopuffer_api_key.clone());
        let file_api_key = nested_api_key.or(legacy_api_key);

        let api_key: Resolved<String> = env_or_resolved(
            "TURBOPUFFER_API_KEY",
            file_or_resolved(file_api_key, String::new()),
        );

        let data_path: Resolved<String> = env_or_resolved(
            "WDPKR_STORE_PATH",
            file_or_resolved(
                f.and_then(|s| s.duckdb.as_ref())
                    .and_then(|d| d.data_path.clone()),
                default_data_path(),
            ),
        );

        (
            Self {
                provider: provider.value,
                turbopuffer: TurbopufferStoreConfig {
                    api_key: api_key.value,
                },
                duckdb: DuckdbStoreConfig {
                    data_path: data_path.value,
                },
            },
            StoreSources {
                provider: provider.source,
                turbopuffer: TurbopufferStoreSources {
                    api_key: api_key.source,
                },
                duckdb: DuckdbStoreSources {
                    data_path: data_path.source,
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
            "WDPKR_STORE_PATH",
            "XDG_DATA_HOME",
        ]);
    }

    #[test]
    #[serial]
    fn defaults() {
        clear_env();
        let cfg = StoreConfig::from_env(&None);
        assert_eq!(cfg.provider, "turbopuffer");
        assert_eq!(cfg.turbopuffer.api_key, "");
        assert!(!cfg.duckdb.data_path.is_empty());
        assert!(cfg.duckdb.data_path.ends_with("wdpkr/duckdb"));
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

    // ── Source attribution ────────────────────────────────────────────

    #[test]
    #[serial]
    fn resolve_marks_default_when_no_input() {
        clear_env();
        let (_, sources) = StoreConfig::resolve(&None);
        assert_eq!(sources.provider, Source::Default);
        assert_eq!(sources.turbopuffer.api_key, Source::Default);
        assert_eq!(sources.duckdb.data_path, Source::Default);
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
        set_env("WDPKR_STORE_PATH", "/custom/path");
        let (_, sources) = StoreConfig::resolve(&None);
        assert_eq!(sources.provider, Source::Env("WDPKR_STORE_PROVIDER"));
        assert_eq!(
            sources.turbopuffer.api_key,
            Source::Env("TURBOPUFFER_API_KEY")
        );
        assert_eq!(sources.duckdb.data_path, Source::Env("WDPKR_STORE_PATH"));
        clear_env();
    }

    // ── Validation ───────────────────────────────────────────────────

    #[test]
    fn validate_passes_turbopuffer_with_key() {
        let cfg = StoreConfig {
            provider: "turbopuffer".into(),
            turbopuffer: TurbopufferStoreConfig {
                api_key: "key-123".into(),
            },
            duckdb: DuckdbStoreConfig {
                data_path: String::new(),
            },
        };
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn validate_fails_turbopuffer_without_key() {
        let cfg = StoreConfig {
            provider: "turbopuffer".into(),
            turbopuffer: TurbopufferStoreConfig {
                api_key: String::new(),
            },
            duckdb: DuckdbStoreConfig {
                data_path: String::new(),
            },
        };
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("TURBOPUFFER_API_KEY"));
    }

    #[test]
    fn validate_fails_unknown_provider() {
        let cfg = StoreConfig {
            provider: "qdrant".into(),
            turbopuffer: TurbopufferStoreConfig {
                api_key: "key".into(),
            },
            duckdb: DuckdbStoreConfig {
                data_path: String::new(),
            },
        };
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("unknown store provider"));
    }

    // ── Backwards compatibility ──────────────────────────────────────

    #[test]
    #[serial]
    fn legacy_flat_api_key_still_resolves() {
        clear_env();
        let file = FileConfig {
            store: Some(FileStoreConfig {
                turbopuffer_api_key: Some("legacy-key".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let cfg = StoreConfig::from_env(&Some(file));
        assert_eq!(cfg.turbopuffer.api_key, "legacy-key");
    }

    #[test]
    #[serial]
    fn nested_key_beats_legacy_flat_key() {
        clear_env();
        let file = FileConfig {
            store: Some(FileStoreConfig {
                turbopuffer_api_key: Some("old-key".into()),
                turbopuffer: Some(FileTurbopufferConfig {
                    api_key: Some("new-key".into()),
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let cfg = StoreConfig::from_env(&Some(file));
        assert_eq!(cfg.turbopuffer.api_key, "new-key");
    }

    #[test]
    #[serial]
    fn legacy_flat_key_source_is_file() {
        clear_env();
        let file = FileConfig {
            store: Some(FileStoreConfig {
                turbopuffer_api_key: Some("key".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let (_, sources) = StoreConfig::resolve(&Some(file));
        assert_eq!(sources.turbopuffer.api_key, Source::File);
    }

    // ── DuckDB config ───────────────────────────────────────────────

    #[test]
    #[serial]
    fn duckdb_data_path_defaults() {
        clear_env();
        let cfg = StoreConfig::from_env(&None);
        assert!(cfg.duckdb.data_path.contains("wdpkr/duckdb"));
    }

    #[test]
    #[serial]
    fn duckdb_env_overrides_data_path() {
        clear_env();
        set_env("WDPKR_STORE_PATH", "/custom/duck");
        let cfg = StoreConfig::from_env(&None);
        assert_eq!(cfg.duckdb.data_path, "/custom/duck");
        clear_env();
    }

    #[test]
    #[serial]
    fn duckdb_file_value_used() {
        clear_env();
        let file = FileConfig {
            store: Some(FileStoreConfig {
                duckdb: Some(FileDuckdbConfig {
                    data_path: Some("/my/data".into()),
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let cfg = StoreConfig::from_env(&Some(file));
        assert_eq!(cfg.duckdb.data_path, "/my/data");
    }

    #[test]
    #[serial]
    fn duckdb_xdg_data_home_respected() {
        clear_env();
        set_env("XDG_DATA_HOME", "/tmp/xdg-data");
        let cfg = StoreConfig::from_env(&None);
        assert_eq!(cfg.duckdb.data_path, "/tmp/xdg-data/wdpkr/duckdb");
        clear_env();
    }
}
