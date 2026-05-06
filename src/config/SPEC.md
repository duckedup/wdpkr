# config

Runtime configuration: defaults → file → env vars → CLI flags.

## Purpose

Resolves all runtime knobs (provider names, model IDs, credentials, indexer
tuning) into a `Config` struct via the `env_or` pattern. Loaded once at
startup; passed by reference everywhere downstream.

## Public surface

Everything here is fully prescribed in root `SPEC.md` § *Configuration*.
This module is largely a transcription of that section.

- `pub struct Config { store, embed, summarizer, indexer }`
- `pub fn Config::new() -> Self` — load + resolve
- `pub struct StoreConfig`
- `pub struct EmbedConfig` (+ `pub fn validate(&self) -> Result<()>`)
- `pub struct SummarizerConfig`
- `pub struct IndexerConfig`
- `pub struct FileConfig` (+ children) — serde target for
  `~/.config/megagrep/config.yaml`
- `pub(crate) fn env_or<T: FromStr>(key: &str, default: T) -> T`
- `pub(crate) fn file_or<T>(file_val: Option<T>, default: T) -> T`

## Files

- `mod.rs` — top-level `Config`, `FileConfig`, `env_or`, `file_or`,
  XDG-aware file loading
- `store.rs` — `StoreConfig` (provider, API key)
- `embed.rs` — `EmbedConfig` (provider, model, batch size, per-provider
  credentials/endpoints, provider-driven default model logic)
- `summarizer.rs` — `SummarizerConfig` (provider, model, API key)
- `indexer.rs` — `IndexerConfig` (namespace, default branch, concurrency,
  max cost, HWM success threshold)

## Plan

1. Implement struct definitions and `from_env(&Option<FileConfig>)` for each
   submodule — code is largely transcription from the SPEC.
2. The full resolution chain at every call site:
   `env_or(KEY, file_or(file.field, hardcoded_default))`.
3. `EmbedConfig`'s default model is **derived from the resolved provider**,
   not a separate constant — see SPEC § `EmbedConfig`.
4. Tests use `serial_test` for env-var isolation (per SPEC § *Testing pattern*).

## Open questions

- Behavior on a malformed config file: silent fallback (current SPEC) vs.
  warn vs. hard error. Current implementation per SPEC: silent (return `None`).
- Whether `Config::new` should call `validate()` on each subconfig itself
  (fail fast at startup) or leave that to the subcommand handler — leaning
  on per-subcommand because `megagrep config get` shouldn't require valid
  credentials.
