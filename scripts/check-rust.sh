#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
cargo fmt --manifest-path core/Cargo.toml --all -- --check
cargo clippy --manifest-path core/Cargo.toml --workspace --all-targets --all-features --locked -- -D warnings
RUSTDOCFLAGS='-D warnings' cargo doc --manifest-path core/Cargo.toml --workspace --no-deps --locked
cargo test --manifest-path core/Cargo.toml --workspace --all-features --locked
cargo audit --file core/Cargo.lock --deny warnings
cargo deny --manifest-path core/Cargo.toml --locked check
