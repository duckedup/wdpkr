//! AI provider adapters — one folder for every hosted/local model backend.
//!
//! Both the embedder and the summarizer pick their concrete implementation
//! from here. Each provider declares which [`Capability`] it supports; the
//! [`PROVIDERS`] registry is the single source of truth the `embed` and
//! `summarize` factories consult before dispatching.
//!
//! - **Voyage / OpenAI / Ollama** — embedders ([`Capability::Embed`])
//! - **Anthropic** — summarizer ([`Capability::Summarize`])
//!
//! The [`Embedder`](crate::embed::Embedder) and
//! [`Summarizer`](crate::summarize::Summarizer) traits live with their
//! consumers in `embed/` and `summarize/`; this module only holds the
//! implementations and shares the retry machinery in [`crate::http`].

pub mod anthropic;
pub mod ollama;
pub mod openai;
pub mod voyage;

/// What a provider can be used for. A provider may support more than one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    Embed,
    Summarize,
}

/// A provider we ship, tagged with the capabilities it offers.
pub struct ProviderInfo {
    pub name: &'static str,
    pub capabilities: &'static [Capability],
}

/// The canonical list of providers wdpkr ships. Voyage is intentionally
/// embed-only — its API has no summarization endpoint.
pub const PROVIDERS: &[ProviderInfo] = &[
    ProviderInfo {
        name: "voyage",
        capabilities: &[Capability::Embed],
    },
    ProviderInfo {
        name: "openai",
        capabilities: &[Capability::Embed],
    },
    ProviderInfo {
        name: "ollama",
        capabilities: &[Capability::Embed],
    },
    ProviderInfo {
        name: "anthropic",
        capabilities: &[Capability::Summarize],
    },
];

/// Whether `name` is a known provider that offers `cap`.
pub fn supports(name: &str, cap: Capability) -> bool {
    PROVIDERS
        .iter()
        .any(|p| p.name == name && p.capabilities.contains(&cap))
}

/// The names of every provider offering `cap`, in registry order — used to
/// build helpful "available providers: ..." error messages.
pub fn names_with(cap: Capability) -> Vec<&'static str> {
    PROVIDERS
        .iter()
        .filter(|p| p.capabilities.contains(&cap))
        .map(|p| p.name)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voyage_embeds_but_does_not_summarize() {
        assert!(supports("voyage", Capability::Embed));
        assert!(!supports("voyage", Capability::Summarize));
    }

    #[test]
    fn anthropic_summarizes_but_does_not_embed() {
        assert!(supports("anthropic", Capability::Summarize));
        assert!(!supports("anthropic", Capability::Embed));
    }

    #[test]
    fn unknown_provider_supports_nothing() {
        assert!(!supports("cohere", Capability::Embed));
        assert!(!supports("cohere", Capability::Summarize));
    }

    #[test]
    fn names_with_lists_in_registry_order() {
        assert_eq!(
            names_with(Capability::Embed),
            vec!["voyage", "openai", "ollama"]
        );
        assert_eq!(names_with(Capability::Summarize), vec!["anthropic"]);
    }
}
