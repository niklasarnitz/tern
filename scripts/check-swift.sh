#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/apple-toolchain.sh
swiftlint lint --strict --config .swiftlint.yml
swiftformat --lint apple/Sources apple/Tests apple/Package.swift --config .swiftformat
bash scripts/generate-bindings.sh
mkdir -p .devbox/checks
fixture_directory=$(mktemp -d "${TMPDIR:-/tmp}/tern-bridge.XXXXXX")
trap 'rm -rf "$fixture_directory"' EXIT
cargo run --manifest-path core/Cargo.toml -p mail-core --example seed_cache --locked -- "$fixture_directory/mail.sqlite"
# Analysis needs actual compiler invocations, so do not reuse an incremental build log.
swift package --package-path apple clean
TERN_TEST_DATABASE="$fixture_directory/mail.sqlite" \
  swift test --package-path apple -v -Xswiftc -warnings-as-errors 2>&1 | tee .devbox/checks/swift-build.log
swiftlint analyze --strict --config .swiftlint.yml --compiler-log-path .devbox/checks/swift-build.log
