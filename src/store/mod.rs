//! wdpkr's vector store backends.
//!
//! [`wdpkr_core::store`] owns the [`VectorStore`] and [`StoreProvider`] traits
//! and the registry that resolves a config string to a backend, but ships no
//! backend of its own — that is what keeps the engine storage-agnostic. This
//! module supplies the two wdpkr uses and re-exports the seam unchanged, so
//! `wdpkr::store::VectorStore` and friends still resolve here.
//!
//! Adding a backend:
//! 1. Implement [`VectorStore`] + [`StoreProvider`] in a new module
//! 2. Declare its settings via [`StoreProvider::settings`] so `store.<name>.*`
//!    resolves through the normal defaults → file → env chain
//! 3. Add one line to [`register_backends`]

pub mod nidus;
pub mod turbopuffer;

pub use wdpkr_core::store::*;

use std::sync::{Arc, Once};

static REGISTER: Once = Once::new();

/// Register wdpkr's backends with core's provider registry. Idempotent.
///
/// **Must run before config resolution.** Core resolves a backend's settings
/// only for providers the registry knows about at the time, so a backend
/// registered afterwards gets no `store.<name>.*` values. Every entry point in
/// [`crate::config`] calls this first; prefer those over core's constructors.
pub fn register_backends() {
    REGISTER.call_once(|| {
        register_provider(Arc::new(turbopuffer::TurbopufferProvider));
        register_provider(Arc::new(nidus::NidusProvider));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Registration is what makes the backends reachable at all — core's
    /// registry starts empty.
    #[test]
    fn register_backends_registers_both() {
        register_backends();
        let names: Vec<String> = registered_providers()
            .iter()
            .map(|p| p.name().to_string())
            .collect();
        assert!(names.contains(&"turbopuffer".to_string()), "{names:?}");
        assert!(names.contains(&"nidus".to_string()), "{names:?}");
    }

    #[test]
    fn resolve_provider_finds_turbopuffer() {
        register_backends();
        assert_eq!(
            resolve_provider("turbopuffer").unwrap().name(),
            "turbopuffer"
        );
        // Provider names are matched case-insensitively.
        assert_eq!(
            resolve_provider("TURBOPUFFER").unwrap().name(),
            "turbopuffer"
        );
    }

    #[test]
    fn resolve_provider_finds_nidus() {
        register_backends();
        assert_eq!(resolve_provider("nidus").unwrap().name(), "nidus");
    }

    #[test]
    fn build_store_builds_turbopuffer() {
        register_backends();
        let config = crate::config::StoreConfig::new("turbopuffer", [("turbopuffer.api_key", "k")]);
        assert!(build_store(&config, 1024).is_ok());
    }

    #[test]
    fn build_store_rejects_unknown_provider() {
        register_backends();
        let config = crate::config::StoreConfig::new("qdrant", [("qdrant.url", "x")]);
        let err = build_store(&config, 1024).err().unwrap().to_string();
        assert!(err.contains("unknown store provider"), "{err}");
    }

    /// Each backend declares the settings core resolves on its behalf — the
    /// keys, env vars, and aliases wdpkr has always honored.
    #[test]
    fn backends_declare_their_settings() {
        register_backends();

        let tp = resolve_provider("turbopuffer").unwrap();
        let api_key = tp.settings().iter().find(|s| s.key == "api_key").unwrap();
        assert_eq!(api_key.env, "TURBOPUFFER_API_KEY");
        assert!(
            api_key.secret,
            "an API key must be withheld from `config list`"
        );
        assert!(
            api_key.file_aliases.contains(&"turbopuffer_api_key"),
            "the deprecated flat key must stay readable"
        );

        let nidus = resolve_provider("nidus").unwrap();
        let path = nidus.settings().iter().find(|s| s.key == "path").unwrap();
        assert_eq!(path.env, "WDPKR_NIDUS_PATH");
        assert!(!path.secret);
        assert!((path.default)().ends_with("wdpkr/nidus"));
    }
}
