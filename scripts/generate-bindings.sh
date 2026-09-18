#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --manifest-path core/Cargo.toml -p mail-core --lib --locked
mkdir -p apple/Generated
cd core
cargo run -p mail-core --bin uniffi-bindgen --locked -- \
  generate --library target/debug/libmail_core.dylib --language swift --out-dir ../apple/Generated
