# cli

Clap-derive-based CLI parsing and subcommand dispatch.

## Purpose

Parses command-line arguments and dispatches to the appropriate handler in the
rest of the crate. This is the only place that knows about clap; downstream
modules receive plain Rust types, never clap structs.

## Public surface

- `pub struct Cli` — top-level parser (clap derive)
- `pub enum Command { Index(IndexArgs), Search(SearchArgs), Config(ConfigArgs), Init(InitArgs) }`
- `pub fn dispatch(cli: Cli) -> anyhow::Result<()>` — entry point from `lib::run`

## Files

- `mod.rs` — `Cli`, `Command`, `dispatch`
- `index.rs` — `IndexArgs` + `run`; mirrors flags in root `SPEC.md` § *megagrep index*
  - `--full`, `--dry-run`, `--concurrency`, `--from`, `--max-cost`
- `search.rs` — `SearchArgs` + `run`; mirrors root `SPEC.md` § *megagrep search*
  - positional `<query>`, `-k/--top-k`, `--symbols-per-file`, `--no-symbols`,
    `--scope`, `--pretty`
- `config.rs` — `ConfigArgs` + `run`; subcommands `init|get|set|list|edit|path`
- `init.rs` — `InitArgs` + `run`; writes `CLAUDE.md` section, `.megagrepignore`,
  GitHub Actions workflow

## Plan

1. Define clap structures matching the SPEC's flag tables verbatim.
2. Each subcommand's `run` function is thin — it builds inputs, calls
   `indexer::IndexRun::run` / `search::SearchRun::run` / etc., and translates
   the result to JSON on stdout, diagnostics on stderr.
3. Map crate errors to exit codes per root `SPEC.md` § *Exit codes*:
   - `0` success, `1` config error (no retry), `2` backend error (transient,
     retry), `3` index missing or empty (fall back to grep).

## Open questions

- Where do `init`'s file templates live (CLAUDE.md section, `.megagrepignore`,
  GH workflow YAML)? Leading candidate: a `src/cli/templates/` directory with
  raw `.md` / `.yml` files pulled in via `include_str!`.
