use super::{FileConfig, Resolved, Source, env_or_resolved, file_or_resolved};

pub struct IndexerConfig {
    /// Override for the auto-derived namespace name. Empty → derive from
    /// `git remote get-url origin` (logic lives in the indexer module).
    pub namespace: String,
    pub default_branch: String,
    pub concurrency: usize,
    pub max_cost: f64,
    pub hwm_success_threshold: f64,
}

#[derive(Debug, Clone)]
pub struct IndexerSources {
    pub namespace: Source,
    pub default_branch: Source,
    pub concurrency: Source,
    pub max_cost: Source,
    pub hwm_success_threshold: Source,
}

impl IndexerConfig {
    pub fn from_env(file: &Option<FileConfig>) -> Self {
        Self::resolve(file).0
    }

    pub fn resolve(file: &Option<FileConfig>) -> (Self, IndexerSources) {
        let f = file.as_ref().and_then(|f| f.indexer.as_ref());

        let namespace: Resolved<String> = env_or_resolved(
            "MEGAGREP_NAMESPACE",
            file_or_resolved(f.and_then(|i| i.namespace.clone()), String::new()),
        );
        let default_branch: Resolved<String> = env_or_resolved(
            "MEGAGREP_DEFAULT_BRANCH",
            file_or_resolved(f.and_then(|i| i.default_branch.clone()), "main".into()),
        );
        let concurrency: Resolved<usize> = env_or_resolved(
            "MEGAGREP_CONCURRENCY",
            file_or_resolved(f.and_then(|i| i.concurrency), 8),
        );
        let max_cost: Resolved<f64> = env_or_resolved(
            "MEGAGREP_MAX_COST",
            file_or_resolved(f.and_then(|i| i.max_cost), 50.0),
        );
        let hwm_success_threshold: Resolved<f64> = env_or_resolved(
            "MEGAGREP_HWM_SUCCESS_THRESHOLD",
            file_or_resolved(f.and_then(|i| i.hwm_success_threshold), 0.95),
        );

        (
            Self {
                namespace: namespace.value,
                default_branch: default_branch.value,
                concurrency: concurrency.value,
                max_cost: max_cost.value,
                hwm_success_threshold: hwm_success_threshold.value,
            },
            IndexerSources {
                namespace: namespace.source,
                default_branch: default_branch.source,
                concurrency: concurrency.source,
                max_cost: max_cost.source,
                hwm_success_threshold: hwm_success_threshold.source,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::FileIndexerConfig;
    use crate::config::test_helpers::{remove_envs, set_env};
    use serial_test::serial;

    fn clear_env() {
        remove_envs(&[
            "MEGAGREP_NAMESPACE",
            "MEGAGREP_DEFAULT_BRANCH",
            "MEGAGREP_CONCURRENCY",
            "MEGAGREP_MAX_COST",
            "MEGAGREP_HWM_SUCCESS_THRESHOLD",
        ]);
    }

    #[test]
    #[serial]
    fn defaults() {
        clear_env();
        let cfg = IndexerConfig::from_env(&None);
        assert_eq!(cfg.namespace, "");
        assert_eq!(cfg.default_branch, "main");
        assert_eq!(cfg.concurrency, 8);
        assert_eq!(cfg.max_cost, 50.0);
        assert_eq!(cfg.hwm_success_threshold, 0.95);
    }

    #[test]
    #[serial]
    fn env_overrides() {
        clear_env();
        set_env("MEGAGREP_CONCURRENCY", "16");
        set_env("MEGAGREP_MAX_COST", "100");
        set_env("MEGAGREP_NAMESPACE", "my-repo");
        let cfg = IndexerConfig::from_env(&None);
        assert_eq!(cfg.concurrency, 16);
        assert_eq!(cfg.max_cost, 100.0);
        assert_eq!(cfg.namespace, "my-repo");
        clear_env();
    }

    #[test]
    #[serial]
    fn parse_failure_uses_default() {
        clear_env();
        set_env("MEGAGREP_CONCURRENCY", "not-a-number");
        let cfg = IndexerConfig::from_env(&None);
        // Per env_or contract: silent fallback on parse failure.
        assert_eq!(cfg.concurrency, 8);
        clear_env();
    }

    #[test]
    #[serial]
    fn float_threshold_parses() {
        clear_env();
        set_env("MEGAGREP_HWM_SUCCESS_THRESHOLD", "0.8");
        let cfg = IndexerConfig::from_env(&None);
        assert!((cfg.hwm_success_threshold - 0.8).abs() < f64::EPSILON);
        clear_env();
    }

    #[test]
    #[serial]
    fn file_values_used_when_env_absent() {
        clear_env();
        let file = FileConfig {
            indexer: Some(FileIndexerConfig {
                namespace: Some("fixed-ns".into()),
                default_branch: Some("trunk".into()),
                concurrency: Some(24),
                max_cost: Some(125.0),
                hwm_success_threshold: Some(0.99),
            }),
            ..Default::default()
        };
        let cfg = IndexerConfig::from_env(&Some(file));
        assert_eq!(cfg.namespace, "fixed-ns");
        assert_eq!(cfg.default_branch, "trunk");
        assert_eq!(cfg.concurrency, 24);
        assert_eq!(cfg.max_cost, 125.0);
        assert!((cfg.hwm_success_threshold - 0.99).abs() < f64::EPSILON);
    }

    #[test]
    #[serial]
    fn env_beats_file() {
        clear_env();
        set_env("MEGAGREP_CONCURRENCY", "4");
        let file = FileConfig {
            indexer: Some(FileIndexerConfig {
                concurrency: Some(24),
                ..Default::default()
            }),
            ..Default::default()
        };
        let cfg = IndexerConfig::from_env(&Some(file));
        assert_eq!(cfg.concurrency, 4);
        clear_env();
    }

    // ── Source attribution ────────────────────────────────────────────

    #[test]
    #[serial]
    fn resolve_marks_default_for_every_field_when_no_input() {
        clear_env();
        let (_, sources) = IndexerConfig::resolve(&None);
        assert_eq!(sources.namespace, Source::Default);
        assert_eq!(sources.default_branch, Source::Default);
        assert_eq!(sources.concurrency, Source::Default);
        assert_eq!(sources.max_cost, Source::Default);
        assert_eq!(sources.hwm_success_threshold, Source::Default);
    }

    #[test]
    #[serial]
    fn resolve_marks_file_when_file_only() {
        clear_env();
        let file = FileConfig {
            indexer: Some(FileIndexerConfig {
                concurrency: Some(24),
                max_cost: Some(125.0),
                ..Default::default()
            }),
            ..Default::default()
        };
        let (_, sources) = IndexerConfig::resolve(&Some(file));
        assert_eq!(sources.concurrency, Source::File);
        assert_eq!(sources.max_cost, Source::File);
        assert_eq!(sources.namespace, Source::Default);
    }

    #[test]
    #[serial]
    fn resolve_marks_env_when_env_set() {
        clear_env();
        set_env("MEGAGREP_CONCURRENCY", "16");
        set_env("MEGAGREP_MAX_COST", "200");
        let (_, sources) = IndexerConfig::resolve(&None);
        assert_eq!(sources.concurrency, Source::Env("MEGAGREP_CONCURRENCY"));
        assert_eq!(sources.max_cost, Source::Env("MEGAGREP_MAX_COST"));
        clear_env();
    }
}
