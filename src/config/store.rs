use super::{FileConfig, Resolved, Source, env_or_resolved, file_or_resolved};

pub struct StoreConfig {
    pub provider: String,
    pub api_key: String,
}

/// Per-field source attribution paralleling [`StoreConfig`].
#[derive(Debug, Clone)]
pub struct StoreSources {
    pub provider: Source,
    pub api_key: Source,
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
            "MEGAGREP_STORE_PROVIDER",
            file_or_resolved(f.and_then(|s| s.provider.clone()), "turbopuffer".into()),
        );
        let api_key: Resolved<String> = env_or_resolved(
            "TURBOPUFFER_API_KEY",
            file_or_resolved(f.and_then(|s| s.turbopuffer_api_key.clone()), String::new()),
        );

        (
            Self {
                provider: provider.value,
                api_key: api_key.value,
            },
            StoreSources {
                provider: provider.source,
                api_key: api_key.source,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::FileStoreConfig;
    use crate::config::test_helpers::{remove_envs, set_env};
    use serial_test::serial;

    fn clear_env() {
        remove_envs(&["MEGAGREP_STORE_PROVIDER", "TURBOPUFFER_API_KEY"]);
    }

    #[test]
    #[serial]
    fn defaults() {
        clear_env();
        let cfg = StoreConfig::from_env(&None);
        assert_eq!(cfg.provider, "turbopuffer");
        assert_eq!(cfg.api_key, "");
    }

    #[test]
    #[serial]
    fn env_overrides_provider() {
        clear_env();
        set_env("MEGAGREP_STORE_PROVIDER", "qdrant");
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
        assert_eq!(cfg.api_key, "test-key-123");
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
        set_env("MEGAGREP_STORE_PROVIDER", "qdrant");
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
        assert_eq!(sources.api_key, Source::Default);
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
        set_env("MEGAGREP_STORE_PROVIDER", "qdrant");
        set_env("TURBOPUFFER_API_KEY", "key");
        let (_, sources) = StoreConfig::resolve(&None);
        assert_eq!(sources.provider, Source::Env("MEGAGREP_STORE_PROVIDER"));
        assert_eq!(sources.api_key, Source::Env("TURBOPUFFER_API_KEY"));
        clear_env();
    }
}
