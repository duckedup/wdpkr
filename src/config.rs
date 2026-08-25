//! Config resolution for the wdpkr CLI.
//!
//! A thin layer over [`wdpkr_core::config`], which it re-exports wholesale.
//! What it adds is the part a *consuming* crate has to supply:
//!
//! 1. **Registration before resolution.** Core resolves `store.<backend>.*`
//!    settings only for backends registered at the time, so every entry point
//!    here registers wdpkr's backends first.
//! 2. **A default backend.** Core cannot prefer a backend it has never heard
//!    of, so it leaves `store.provider` empty. wdpkr has always defaulted to
//!    Turbopuffer, and re-applies that here.
//!
//! Use these functions rather than [`Config::new`] / [`ResolvedConfig::new`]
//! directly — the bare core constructors skip both steps.

pub use wdpkr_core::config::*;

use anyhow::Result;

/// The backend wdpkr uses when config names none.
pub const DEFAULT_STORE_PROVIDER: &str = "turbopuffer";

/// Resolve config: defaults → `~/.config/wdpkr/config.yaml` → env vars.
///
/// Errors only if the config file exists but is malformed; a missing file is
/// not an error. See [`Config::new`].
pub fn load() -> Result<Config> {
    Ok(resolve()?.config)
}

/// Resolve from an explicit (possibly absent) on-disk config, bypassing the
/// real `~/.config/...` lookup. Primarily for tests.
pub fn load_from_file(file: Option<FileConfig>) -> Config {
    resolve_from_file(file).config
}

/// [`load`], keeping per-field source attribution. Drives `wdpkr config list`.
pub fn resolve() -> Result<ResolvedConfig> {
    crate::store::register_backends();
    Ok(with_default_provider(ResolvedConfig::new()?))
}

/// [`load_from_file`], keeping per-field source attribution.
pub fn resolve_from_file(file: Option<FileConfig>) -> ResolvedConfig {
    crate::store::register_backends();
    with_default_provider(ResolvedConfig::from_file(file))
}

/// Fill in wdpkr's default backend when nothing selected one.
fn with_default_provider(mut resolved: ResolvedConfig) -> ResolvedConfig {
    if resolved.config.store.provider.trim().is_empty() {
        resolved.config.store.provider = DEFAULT_STORE_PROVIDER.into();
        // The value is now a hardcoded fallback, whatever an empty env var or
        // file entry claimed a moment ago.
        resolved.sources.store.provider = Source::Default;
    }
    resolved
}

/// Test-only helpers for env-var manipulation.
///
/// Mirrors `wdpkr_core::config::test_helpers`, which is crate-private to core.
/// Edition 2024 made `set_var`/`remove_var` `unsafe` because they can race
/// with reads on other threads; every caller here must be `#[serial]`, which
/// serializes the tests that touch env state.
#[cfg(test)]
pub(crate) mod test_helpers {
    pub fn set_env(key: &str, val: &str) {
        // SAFETY: callers are `#[serial]`; no concurrent env access can race
        // with this mutation.
        unsafe { std::env::set_var(key, val) };
    }

    pub fn remove_env(key: &str) {
        // SAFETY: callers are `#[serial]`.
        unsafe { std::env::remove_var(key) };
    }

    pub fn remove_envs(keys: &[&str]) {
        for key in keys {
            remove_env(key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_helpers::{remove_env, set_env};
    use super::*;
    use serial_test::serial;

    /// `store.provider` is not set anywhere, so wdpkr supplies its own.
    #[test]
    #[serial]
    fn provider_defaults_to_turbopuffer() {
        remove_env("WDPKR_STORE_PROVIDER");
        let resolved = resolve_from_file(None);
        assert_eq!(resolved.config.store.provider, "turbopuffer");
        assert_eq!(resolved.sources.store.provider, Source::Default);
    }

    #[test]
    #[serial]
    fn explicit_provider_wins_over_the_default() {
        remove_env("WDPKR_STORE_PROVIDER");
        let file = FileConfig {
            store: Some(FileStoreConfig {
                provider: Some("nidus".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let resolved = resolve_from_file(Some(file));
        assert_eq!(resolved.config.store.provider, "nidus");
        assert_eq!(resolved.sources.store.provider, Source::File);
    }

    /// An env var set to the empty string selects no backend, so the default
    /// still applies — and the attribution says `default`, not `env`.
    #[test]
    #[serial]
    fn empty_env_provider_falls_back_to_the_default() {
        set_env("WDPKR_STORE_PROVIDER", "");
        let resolved = resolve_from_file(None);
        assert_eq!(resolved.config.store.provider, "turbopuffer");
        assert_eq!(resolved.sources.store.provider, Source::Default);
        remove_env("WDPKR_STORE_PROVIDER");
    }

    /// Resolution registers the backends, so their settings resolve too —
    /// this is what makes `store.nidus.path` land in the config at all.
    #[test]
    #[serial]
    fn backend_settings_resolve_because_registration_happened_first() {
        remove_env("WDPKR_NIDUS_PATH");
        let config = load_from_file(None);
        assert!(
            config.store.get("nidus.path").ends_with("wdpkr/nidus"),
            "nidus path: {}",
            config.store.get("nidus.path")
        );
    }
}
