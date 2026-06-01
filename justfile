# wdpkr — task runner.
# Run `just` (or `just --list`) to see available recipes.

# Default: list recipes
default:
    @just --list

# ── Local dev ──────────────────────────────────────────────────────────────

# Run wdpkr with arguments (e.g. `just run search "foo"`)
run *ARGS:
    cargo run -- {{ ARGS }}

# Quick compile check across the workspace
check:
    cargo check --all-targets

# Format all code
fmt:
    cargo fmt --all

# Verify formatting is clean (CI guard)
fmt-check:
    cargo fmt --all -- --check

# Lint with clippy, deny all warnings
lint:
    cargo clippy --all-targets --all-features -- -D warnings

# Run all tests
test:
    cargo test --all-features

# ── Docs site (Astro + Starlight, in docs/) ─────────────────────────────────

# Run the docs dev server with live reload (installs deps on first run)
docs:
    cd docs && bun install && bun run dev

# Build the docs site to docs/dist/
docs-build:
    cd docs && bun install && bun run build

# Preview the production docs build locally
docs-preview:
    cd docs && bun run preview

# Run tests within a single module/path (e.g. `just test-mod config`)
test-mod MOD:
    cargo test --all-features {{ MOD }}

# Run Miri to check for undefined behavior (requires nightly).
# --no-default-features drops the DuckDB backend (bundled C library FFI that
# Miri cannot execute).
miri:
    MIRIFLAGS="-Zmiri-disable-isolation -Zmiri-permissive-provenance -Zmiri-ignore-leaks" cargo +nightly miri test --no-default-features

# Pre-commit / pre-PR checks: format clean, no clippy warnings, tests green
ci: fmt-check lint test

# ── Build ──────────────────────────────────────────────────────────────────

# Debug build for the current host
build:
    cargo build

# Release build for the current host
release:
    cargo build --release

# ── Cross-platform release ─────────────────────────────────────────────────
# Targets per SPEC.md § Distribution: linux x86_64, linux arm64, macOS arm64.
# Linux targets use `cross` (requires Docker); macOS targets build natively
# on macOS hosts via rustup-installed targets.

# Install rustup targets for native cross-compilation (macOS)
install-targets:
    rustup target add aarch64-apple-darwin x86_64-apple-darwin

# Install `cross` (used for Linux targets from a non-Linux host)
install-cross:
    cargo install cross --git https://github.com/cross-rs/cross

# Build all release binaries listed in SPEC.md § Distribution
release-all: release-macos-arm64 release-linux-x86_64 release-linux-arm64

# macOS arm64 (Apple Silicon)
release-macos-arm64:
    rustup target add aarch64-apple-darwin
    cargo build --release --target aarch64-apple-darwin

# Linux x86_64 — uses `cross` (requires Docker)
release-linux-x86_64:
    cross build --release --target x86_64-unknown-linux-gnu

# Linux arm64 — uses `cross` (requires Docker)
release-linux-arm64:
    cross build --release --target aarch64-unknown-linux-gnu

# Package built release binaries into target/release-archives/ as tar.gz
package: release-all
    #!/usr/bin/env bash
    set -euo pipefail
    OUT=target/release-archives
    mkdir -p "$OUT"
    for target in aarch64-apple-darwin x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu; do
        binary="target/$target/release/wdpkr"
        if [ -f "$binary" ]; then
            archive="$OUT/wdpkr-$target.tar.gz"
            tar -czf "$archive" -C "target/$target/release" wdpkr
            echo "Packaged $archive"
        else
            echo "Skipping $target — binary missing at $binary" >&2
        fi
    done

# Install the binary to ~/.cargo/bin from this checkout
install:
    cargo install --path .

# Initialize beads issue tracking for this project
bd-init:
    bd init --reinit-local --prefix wdpkr
    git config beads.role contributor
    chmod 700 .beads

# Remove all build artifacts
clean:
    cargo clean
