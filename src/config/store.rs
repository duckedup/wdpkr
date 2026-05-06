use super::{FileConfig, env_or, file_or};

pub struct StoreConfig {
    pub provider: String,
    pub api_key: String,
}

impl StoreConfig {
    pub fn from_env(file: &Option<FileConfig>) -> Self {
        let f = file.as_ref().and_then(|f| f.store.as_ref());
        Self {
            provider: env_or(
                "MEGAGREP_STORE_PROVIDER",
                file_or(f.and_then(|s| s.provider.clone()), "turbopuffer".into()),
            ),
            api_key: env_or("TURBOPUFFER_API_KEY", String::new()),
        }
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
            }),
            ..Default::default()
        };
        let cfg = StoreConfig::from_env(&Some(file));
        assert_eq!(cfg.provider, "qdrant");
        clear_env();
    }
}
